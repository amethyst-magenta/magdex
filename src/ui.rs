use std::ops::Range;

use ratatui::style::Stylize;
use ratatui::{
    buffer::{Buffer, Cell},
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{format_duration, relative_time, SLASH_COMMANDS},
    model::{
        display_path, layout_composer, ActionStatus, AppState, ApprovalKind, BlockKind,
        CommandAction, CommandActionKind, ComposerLayout, ContextUsage, CopyMode, FileChange,
        FileChangeKind, ImageAttachment, Popup, QueuedTurn, QuotaUsage, ResumeScope, ThreadSummary,
        TranscriptBlock, TranscriptHyperlink, TrustDirectoryPrompt, UserInputRequest,
        VisibleHyperlink,
    },
};

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const USER_BACKGROUND: Color = Color::Rgb(48, 48, 48);
const COMPOSER_BACKGROUND: Color = Color::Rgb(38, 38, 38);
const DIFF_ADD_BACKGROUND: Color = Color::Rgb(28, 65, 46);
const DIFF_REMOVE_BACKGROUND: Color = Color::Rgb(78, 37, 34);
const COPY_ANSWER_BACKGROUND: Color = Color::Rgb(25, 43, 45);
const COPY_CURSOR_BACKGROUND: Color = Color::Rgb(38, 67, 70);
const COPY_SELECTED_BACKGROUND: Color = Color::Rgb(35, 51, 72);
const COPY_SELECTED_CURSOR_BACKGROUND: Color = Color::Rgb(47, 72, 91);
const COLLAPSED_COMMAND_ROWS: usize = 3;
const HEADER_GROUP_GAP: usize = 4;

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    state.visible_hyperlinks.clear();
    if state.popup.is_some() && state.copy_mode.take().is_some() {
        state.mark_transcript_dirty();
    }
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
    let queued_rows = queued_turn_row_count(state.queued_turns.len());
    let slash_suggestions = slash_command_suggestions(&state.composer.text);
    let suggestion_rows = slash_suggestions.len() as u16;
    let composer_height = (composer_lines + attachment_rows + queued_rows + suggestion_rows + 4)
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
    if let Some(Popup::Approval(approval)) = state.popup.as_mut() {
        update_approval_scroll_bounds(approval, chunks[3]);
    }
    if let Some(popup) = &state.popup {
        draw_bottom_panel(frame, state, popup.clone(), chunks[3]);
    } else {
        draw_composer(frame, state, &composer, &slash_suggestions, chunks[3]);
    }
}

pub(crate) fn expected_transcript_viewport_height(state: &AppState, area: Rect) -> Option<u16> {
    if state.resume_picker.is_some() {
        return None;
    }
    let composer_width = area.width.saturating_sub(6).max(1) as usize;
    let composer = layout_composer(&state.composer.text, state.composer.cursor, composer_width);
    let composer_lines = composer.lines.len().clamp(1, 8) as u16;
    let attachment_rows = attachment_row_count(state.image_attachments.len());
    let queued_rows = queued_turn_row_count(state.queued_turns.len());
    let suggestion_rows = slash_command_suggestions(&state.composer.text).len() as u16;
    let composer_height = (composer_lines + attachment_rows + queued_rows + suggestion_rows + 4)
        .min(area.height.saturating_sub(4).max(5));
    let panel_gap = u16::from(state.popup.is_some() && area.height > 4);
    let bottom_height = state
        .popup
        .as_ref()
        .map(|popup| bottom_panel_height(state, popup, area.width))
        .unwrap_or(composer_height)
        .min(area.height.saturating_sub(5 + panel_gap).max(1));
    Some(
        area.height
            .saturating_sub(4)
            .saturating_sub(panel_gap)
            .saturating_sub(bottom_height),
    )
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
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let scope_title = match picker.scope {
        ResumeScope::CurrentDirectory => "current",
        ResumeScope::AllDirectories => "all",
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("Recent conversations · {scope_title}"),
            Style::default().bold(),
        )),
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
        let message = match picker.scope {
            ResumeScope::CurrentDirectory => "No conversations found in this directory",
            ResumeScope::AllDirectories => "No conversations found",
        };
        vec![ListItem::new(message)]
    } else {
        let width = chunks[1].width.saturating_sub(2) as usize;
        state
            .threads
            .iter()
            .map(|thread| {
                let (title, metadata) = resume_entry_parts(thread, width, picker.scope);
                ListItem::new(Line::from(vec![
                    Span::raw(title),
                    Span::styled(metadata, Style::default().fg(DIM)),
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
        Paragraph::new("Tab current/all · j/k or ↑/↓ move · Enter resume · Ctrl+C quit")
            .style(Style::default().fg(DIM)),
        chunks[2],
    );
}

fn draw_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let metadata = [
        state.model.as_deref().unwrap_or("default"),
        state.effort.as_deref().unwrap_or("default"),
    ]
    .join(" · ");
    let content_width = area.width.saturating_sub(2) as usize;
    let quota = header_quota_text(&state.quota_usage);
    let context_variants = header_context_variants(state.context_usage.as_ref());
    let context_with_quota = context_variants.iter().find(|context| {
        metadata.width() + quota.width() + context.width() + HEADER_GROUP_GAP * 2 <= content_width
    });
    let quota_fits =
        !quota.is_empty() && metadata.width() + quota.width() + HEADER_GROUP_GAP <= content_width;
    let (quota, context) = if !quota.is_empty() {
        if let Some(context) = context_with_quota {
            (quota, (*context).clone())
        } else if quota_fits {
            (quota, String::new())
        } else {
            let context = context_variants
                .into_iter()
                .find(|context| {
                    metadata.width() + context.width() + HEADER_GROUP_GAP <= content_width
                })
                .unwrap_or_default();
            (String::new(), context)
        }
    } else {
        let context = context_variants
            .into_iter()
            .find(|context| metadata.width() + context.width() + HEADER_GROUP_GAP <= content_width)
            .unwrap_or_default();
        (String::new(), context)
    };
    let free = content_width.saturating_sub(metadata.width() + quota.width() + context.width());
    let mut spans = vec![Span::styled(metadata, Style::default().fg(DIM))];
    match (quota.is_empty(), context.is_empty()) {
        (false, false) => {
            let first_gap = free / 2;
            spans.push(Span::raw(" ".repeat(first_gap)));
            spans.push(Span::styled(quota, Style::default().fg(DIM)));
            spans.push(Span::raw(" ".repeat(free.saturating_sub(first_gap))));
        }
        (false, true) => {
            spans.push(Span::raw(" ".repeat(free)));
            spans.push(Span::styled(quota, Style::default().fg(DIM)));
        }
        (true, false) => spans.push(Span::raw(" ".repeat(free))),
        (true, true) => {}
    }
    if !context.is_empty() {
        spans.push(Span::styled(context, Style::default().fg(DIM)));
    }
    let line = Line::from(spans);
    let text_area = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        1,
    );
    frame.render_widget(Paragraph::new(line), text_area);

    let mode_title = format!(" {} ", display_mode(&state.collaboration_mode));
    let project_title = format!(" {} ", state.project);
    let show_project =
        mode_title.width() + project_title.width() + HEADER_GROUP_GAP <= area.width as usize;
    let mut separator = Block::default()
        .title(Line::styled(mode_title, Style::default().fg(DIM).bold()).left_aligned())
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::White));
    if show_project {
        separator = separator
            .title(Line::styled(project_title, Style::default().fg(DIM).bold()).right_aligned());
    }
    let separator_area = Rect::new(area.x, area.y.saturating_add(3), area.width, 1);
    frame.render_widget(separator, separator_area);
}

fn header_context_variants(usage: Option<&ContextUsage>) -> Vec<String> {
    let Some(usage) = usage else {
        return Vec::new();
    };
    let input = compact_token_count(usage.input_tokens);
    let percent = usage
        .context_window
        .filter(|window| *window > 0)
        .map(|window| (usage.input_tokens as f64 * 100.0 / window as f64).round() as u64);
    match percent {
        Some(percent) => vec![
            format!("{input} ({percent}%)"),
            format!("{input} {percent}%"),
        ],
        None => vec![input],
    }
}

fn header_quota_text(usage: &QuotaUsage) -> String {
    let five_hour = usage.five_hour.map(|window| window.remaining_percent());
    let weekly = usage.weekly.map(|window| window.remaining_percent());
    match (five_hour, weekly) {
        (Some(five_hour), Some(weekly)) => format!("5h {five_hour}% · week {weekly}%"),
        (Some(five_hour), None) => format!("5h {five_hour}%"),
        (None, Some(weekly)) => format!("week {weekly}%"),
        (None, None) => String::new(),
    }
}

fn display_mode(mode: &str) -> String {
    let mut chars = mode.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().chain(chars).collect()
}

fn compact_token_count(value: u64) -> String {
    match value {
        0..=999 => value.to_string(),
        1_000..=999_999 => compact_decimal(value as f64 / 1_000.0, "k"),
        _ => compact_decimal(value as f64 / 1_000_000.0, "m"),
    }
}

fn compact_decimal(value: f64, suffix: &str) -> String {
    let mut value = format!("{value:.1}");
    if value.ends_with(".0") {
        value.truncate(value.len() - 2);
    }
    format!("{value}{suffix}")
}

fn draw_transcript(frame: &mut Frame, state: &mut AppState, area: Rect) {
    state.transcript_viewport_height = area.height as usize;
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
    if state.at_bottom || state.scroll == usize::MAX || state.scroll >= max_scroll {
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
    state.visible_hyperlinks = state
        .transcript_hyperlinks
        .iter()
        .filter_map(|link| {
            if !(scroll..visible_end).contains(&link.line) {
                return None;
            }
            let row = area.y.saturating_add((link.line - scroll) as u16);
            let start = area.x.saturating_add(link.start as u16);
            let end = area.x.saturating_add(link.end as u16).min(area.right());
            (start < end).then(|| VisibleHyperlink {
                row,
                start,
                end,
                url: link.url.clone(),
            })
        })
        .collect();
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalOverlayCell {
    pub position: Position,
    pub cell: Cell,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalHyperlinkOverlay {
    pub url: String,
    pub cells: Vec<TerminalOverlayCell>,
}

pub(crate) fn terminal_hyperlink_overlay(
    buffer: &Buffer,
    links: &[VisibleHyperlink],
) -> Vec<TerminalHyperlinkOverlay> {
    links
        .iter()
        .filter_map(|link| {
            let cells = (link.start..link.end)
                .filter_map(|column| {
                    let position = Position::new(column, link.row);
                    let cell = buffer.cell(position)?;
                    (!cell.skip).then(|| TerminalOverlayCell {
                        position,
                        cell: cell.clone(),
                    })
                })
                .collect::<Vec<_>>();
            (!cells.is_empty()).then(|| TerminalHyperlinkOverlay {
                url: link.url.clone(),
                cells,
            })
        })
        .collect()
}

pub(crate) fn terminal_hyperlink_cleanup(
    buffer: &Buffer,
    previous: &[TerminalHyperlinkOverlay],
) -> Vec<TerminalOverlayCell> {
    let mut cells = Vec::new();
    for position in previous
        .iter()
        .flat_map(|link| link.cells.iter().map(|cell| cell.position))
    {
        if cells
            .iter()
            .any(|cell: &TerminalOverlayCell| cell.position == position)
        {
            continue;
        }
        let Some(cell) = buffer.cell(position) else {
            continue;
        };
        if !cell.skip {
            cells.push(TerminalOverlayCell {
                position,
                cell: cell.clone(),
            });
        }
    }
    cells
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
    let mut markdown_ranges = vec![vec![]; state.blocks.len()];
    let mut hyperlinks = Vec::new();
    let copy_mode = state.copy_mode.clone();
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
        markdown_ranges[index] = append_block_with_copy_mode(
            &mut lines,
            &mut hyperlinks,
            block,
            BlockRenderContext {
                width: content_width as usize,
                render_width: render_width as usize,
                cwd: &state.cwd,
                can_expand: current_action == Some(index) && block.kind == BlockKind::Command,
                copy_mode: copy_mode.as_ref(),
                block_index: index,
            },
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
    state.transcript_markdown_ranges = markdown_ranges;
    state.transcript_hyperlinks = hyperlinks;
}

#[cfg(test)]
fn append_block(
    lines: &mut Vec<Line<'static>>,
    block: &TranscriptBlock,
    width: usize,
    render_width: usize,
    cwd: &str,
    can_expand: bool,
) {
    append_block_with_copy_mode(
        lines,
        &mut Vec::new(),
        block,
        BlockRenderContext {
            width,
            render_width,
            cwd,
            can_expand,
            copy_mode: None,
            block_index: usize::MAX,
        },
    );
}

struct BlockRenderContext<'a> {
    width: usize,
    render_width: usize,
    cwd: &'a str,
    can_expand: bool,
    copy_mode: Option<&'a CopyMode>,
    block_index: usize,
}

fn append_block_with_copy_mode(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    block: &TranscriptBlock,
    context: BlockRenderContext<'_>,
) -> Vec<(usize, usize)> {
    let BlockRenderContext {
        width,
        render_width,
        cwd,
        can_expand,
        copy_mode,
        block_index,
    } = context;
    let selected_answer = copy_mode.is_some_and(|mode| match mode {
        CopyMode::Answers {
            block_index: selected,
        }
        | CopyMode::Markdown {
            block_index: selected,
            ..
        } => *selected == block_index,
    });
    let markdown_selection = copy_mode.and_then(|mode| match mode {
        CopyMode::Markdown {
            block_index: selected,
            markdown_index,
            selected: marked,
        } if *selected == block_index => Some((*markdown_index, marked.as_slice())),
        _ => None,
    });
    let mut markdown_ranges = Vec::new();
    match block.kind {
        BlockKind::User => {
            let start = lines.len();
            lines.push(Line::default());
            append_markdown_with_hyperlinks(
                lines,
                hyperlinks,
                &block.text,
                "  ",
                Style::default(),
                width,
            );
            lines.push(Line::default());
            apply_background_band(&mut lines[start..], render_width, USER_BACKGROUND);
        }
        BlockKind::Assistant => {
            markdown_ranges = append_markdown_with_copy_mode(
                lines,
                hyperlinks,
                &block.text,
                MarkdownRenderContext {
                    indent: "  ",
                    base_style: Style::default(),
                    width,
                    render_width,
                    selected_answer,
                    selection: markdown_selection,
                },
            );
        }
        BlockKind::Commentary => {
            append_markdown_with_hyperlinks(
                lines,
                hyperlinks,
                &block.text,
                "  ",
                Style::default().fg(DIM),
                width,
            );
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
        BlockKind::Compaction => {
            lines.push(Line::from(Span::styled(
                format!("  {}", block.title),
                Style::default().bold(),
            )));
            push_wrapped_line(
                lines,
                vec![Span::styled(
                    format!("    {}", block.text),
                    Style::default().fg(DIM),
                )],
                width,
            );
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
            if block.title == "Approval" {
                let (verb, color) = if block.text.starts_with("You approved ") {
                    ("approved", Color::Green)
                } else {
                    ("denied", Color::Red)
                };
                let (before, after) = block.text.split_once(verb).unwrap_or(("", &block.text));
                push_wrapped_line(
                    lines,
                    vec![
                        Span::styled(format!("  {before}"), Style::default().fg(DIM)),
                        Span::styled(verb.to_string(), Style::default().fg(color).bold()),
                        Span::styled(after.to_string(), Style::default().fg(DIM)),
                    ],
                    width,
                );
            } else {
                push_wrapped_line(
                    lines,
                    vec![
                        Span::styled("  · ", Style::default().fg(DIM)),
                        Span::styled(block.text.clone(), Style::default().fg(DIM)),
                    ],
                    width,
                );
            }
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
    markdown_ranges
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
        let mut command_rows = Vec::new();
        push_wrapped_line(&mut command_rows, spans, width);
        append_collapsible_command(lines, command_rows, block.expanded, can_expand, width);
    }

    let failed = status == ActionStatus::Failed || block.exit_code.is_some_and(|code| code != 0);
    let expanded = block.expanded;
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

fn append_collapsible_command(
    lines: &mut Vec<Line<'static>>,
    rows: Vec<Line<'static>>,
    expanded: bool,
    show_shortcut: bool,
    width: usize,
) {
    let hidden = rows.len().saturating_sub(COLLAPSED_COMMAND_ROWS);
    if expanded || hidden == 0 {
        lines.extend(rows);
        return;
    }

    let leading_rows = COLLAPSED_COMMAND_ROWS.saturating_sub(1);
    lines.extend(rows.iter().take(leading_rows).cloned());
    let mut summary = if show_shortcut {
        format!("    … +{hidden} lines · Ctrl+O to expand")
    } else {
        format!("    … +{hidden} lines")
    };
    if summary.width() > width && show_shortcut {
        summary = format!("    … +{hidden} · Ctrl+O");
    }
    let summary = truncate(&summary, width);
    lines.push(Line::from(Span::styled(summary, Style::default().fg(DIM))));
    if let Some(last) = rows.last() {
        lines.push(last.clone());
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

#[cfg(test)]
fn append_markdown(
    lines: &mut Vec<Line<'static>>,
    source: &str,
    indent: &str,
    base_style: Style,
    width: usize,
) {
    append_markdown_with_hyperlinks(lines, &mut Vec::new(), source, indent, base_style, width);
}

fn append_markdown_with_hyperlinks(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    source: &str,
    indent: &str,
    base_style: Style,
    width: usize,
) {
    append_markdown_with_copy_mode(
        lines,
        hyperlinks,
        source,
        MarkdownRenderContext {
            indent,
            base_style,
            width,
            render_width: width,
            selected_answer: false,
            selection: None,
        },
    );
}

struct MarkdownRenderContext<'a> {
    indent: &'a str,
    base_style: Style,
    width: usize,
    render_width: usize,
    selected_answer: bool,
    selection: Option<(usize, &'a [usize])>,
}

fn append_markdown_with_copy_mode(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    source: &str,
    context: MarkdownRenderContext<'_>,
) -> Vec<(usize, usize)> {
    let MarkdownRenderContext {
        indent,
        base_style,
        width,
        render_width,
        selected_answer,
        selection,
    } = context;
    let blocks = parse_markdown_blocks(source);
    let mut rendered_ranges = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
            if selected_answer {
                let last = lines.len() - 1;
                apply_background_band(&mut lines[last..], render_width, COPY_ANSWER_BACKGROUND);
            }
        }
        let start = lines.len();
        render_markdown_block(lines, hyperlinks, source, block, indent, base_style, width);
        let end = lines.len();
        if block.kind == MarkdownBlockKind::Rule {
            if selected_answer {
                apply_background_band(&mut lines[start..end], render_width, COPY_ANSWER_BACKGROUND);
            }
            continue;
        }

        let markdown_index = rendered_ranges.len();
        rendered_ranges.push((start, end));
        let background = selection
            .map(|(current, selected)| {
                let focused = current == markdown_index;
                let marked = selected.binary_search(&markdown_index).is_ok();
                match (focused, marked) {
                    (true, true) => COPY_SELECTED_CURSOR_BACKGROUND,
                    (true, false) => COPY_CURSOR_BACKGROUND,
                    (false, true) => COPY_SELECTED_BACKGROUND,
                    (false, false) => COPY_ANSWER_BACKGROUND,
                }
            })
            .or(selected_answer.then_some(COPY_ANSWER_BACKGROUND));
        if let Some(background) = background {
            apply_background_band(&mut lines[start..end], render_width, background);
        }
    }
    rendered_ranges
}

pub(crate) fn markdown_copy_ranges(source: &str) -> Vec<Range<usize>> {
    parse_markdown_blocks(source)
        .into_iter()
        .filter_map(|block| match block.kind {
            MarkdownBlockKind::Rule => None,
            MarkdownBlockKind::Code { .. } => {
                let mut range = block.content_range;
                if source[range.clone()].ends_with("\r\n") {
                    range.end = range.end.saturating_sub(2);
                } else if source[range.clone()].ends_with('\n') {
                    range.end = range.end.saturating_sub(1);
                }
                Some(range)
            }
            _ => Some(block.range),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MarkdownBlockKind {
    Heading(u8),
    Paragraph,
    Code { language: String },
    List,
    Quote,
    Table,
    Rule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MarkdownBlock {
    kind: MarkdownBlockKind,
    range: Range<usize>,
    content_range: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
struct MarkdownSourceLine<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    full_end: usize,
}

fn parse_markdown_blocks(source: &str) -> Vec<MarkdownBlock> {
    let source_lines = markdown_source_lines(source);
    let mut blocks = Vec::new();
    let mut index = 0;

    while index < source_lines.len() {
        if source_lines[index].text.trim().is_empty() {
            index += 1;
            continue;
        }

        let first = source_lines[index];
        if let Some((fence_length, language)) = markdown_fence(first.text) {
            let content_start = first.full_end;
            let mut end_index = index + 1;
            while end_index < source_lines.len()
                && !is_closing_markdown_fence(source_lines[end_index].text, fence_length)
            {
                end_index += 1;
            }
            let (range_end, content_end, next_index) = if end_index < source_lines.len() {
                (
                    source_lines[end_index].end,
                    source_lines[end_index].start,
                    end_index + 1,
                )
            } else {
                (source.len(), source.len(), source_lines.len())
            };
            blocks.push(MarkdownBlock {
                kind: MarkdownBlockKind::Code {
                    language: language.to_string(),
                },
                range: first.start..range_end,
                content_range: content_start.min(content_end)..content_end,
            });
            index = next_index;
            continue;
        }

        if let Some((level, content_offset)) = markdown_heading(first.text) {
            blocks.push(MarkdownBlock {
                kind: MarkdownBlockKind::Heading(level),
                range: first.start..first.end,
                content_range: first.start + content_offset..first.end,
            });
            index += 1;
            continue;
        }

        if is_markdown_rule(first.text) {
            blocks.push(MarkdownBlock {
                kind: MarkdownBlockKind::Rule,
                range: first.start..first.end,
                content_range: first.start..first.end,
            });
            index += 1;
            continue;
        }

        if is_markdown_table_start(&source_lines, index) {
            let mut end_index = index + 2;
            while end_index < source_lines.len()
                && !source_lines[end_index].text.trim().is_empty()
                && source_lines[end_index].text.contains('|')
            {
                end_index += 1;
            }
            let last = source_lines[end_index - 1];
            blocks.push(MarkdownBlock {
                kind: MarkdownBlockKind::Table,
                range: first.start..last.end,
                content_range: first.start..last.end,
            });
            index = end_index;
            continue;
        }

        if markdown_list_item(first.text).is_some() {
            let mut end_index = index + 1;
            while end_index < source_lines.len() {
                let line = source_lines[end_index].text;
                if line.trim().is_empty() {
                    break;
                }
                if markdown_list_item(line).is_some() || markdown_leading_width(line) > 0 {
                    end_index += 1;
                } else {
                    break;
                }
            }
            let last = source_lines[end_index - 1];
            blocks.push(MarkdownBlock {
                kind: MarkdownBlockKind::List,
                range: first.start..last.end,
                content_range: first.start..last.end,
            });
            index = end_index;
            continue;
        }

        if first.text.trim_start().starts_with('>') {
            let mut end_index = index + 1;
            while end_index < source_lines.len()
                && source_lines[end_index].text.trim_start().starts_with('>')
            {
                end_index += 1;
            }
            let last = source_lines[end_index - 1];
            blocks.push(MarkdownBlock {
                kind: MarkdownBlockKind::Quote,
                range: first.start..last.end,
                content_range: first.start..last.end,
            });
            index = end_index;
            continue;
        }

        let mut end_index = index + 1;
        while end_index < source_lines.len()
            && !source_lines[end_index].text.trim().is_empty()
            && !starts_markdown_block(&source_lines, end_index)
        {
            end_index += 1;
        }
        let last = source_lines[end_index - 1];
        blocks.push(MarkdownBlock {
            kind: MarkdownBlockKind::Paragraph,
            range: first.start..last.end,
            content_range: first.start..last.end,
        });
        index = end_index;
    }

    blocks
}

fn markdown_source_lines(source: &str) -> Vec<MarkdownSourceLine<'_>> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for raw in source.split_inclusive('\n') {
        let without_newline = raw.strip_suffix('\n').unwrap_or(raw);
        let text = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        lines.push(MarkdownSourceLine {
            text,
            start: offset,
            end: offset + text.len(),
            full_end: offset + raw.len(),
        });
        offset += raw.len();
    }
    if source.is_empty() {
        return lines;
    }
    if offset < source.len() {
        let text = &source[offset..];
        lines.push(MarkdownSourceLine {
            text,
            start: offset,
            end: source.len(),
            full_end: source.len(),
        });
    }
    lines
}

fn starts_markdown_block(lines: &[MarkdownSourceLine<'_>], index: usize) -> bool {
    let line = lines[index].text;
    markdown_fence(line).is_some()
        || markdown_heading(line).is_some()
        || is_markdown_rule(line)
        || is_markdown_table_start(lines, index)
        || markdown_list_item(line).is_some()
        || line.trim_start().starts_with('>')
}

fn markdown_fence(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let fence_length = trimmed
        .chars()
        .take_while(|character| *character == '`')
        .count();
    (fence_length >= 3).then(|| (fence_length, trimmed[fence_length..].trim()))
}

fn is_closing_markdown_fence(line: &str, opening_length: usize) -> bool {
    let trimmed = line.trim();
    let fence_length = trimmed
        .chars()
        .take_while(|character| *character == '`')
        .count();
    fence_length >= opening_length && trimmed[fence_length..].trim().is_empty()
}

fn markdown_heading(line: &str) -> Option<(u8, usize)> {
    let leading = line.len() - line.trim_start_matches(' ').len();
    if leading > 3 {
        return None;
    }
    let trimmed = &line[leading..];
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) || !trimmed[level..].starts_with(' ') {
        return None;
    }
    Some((level as u8, leading + level + 1))
}

fn is_markdown_rule(line: &str) -> bool {
    let compact = line
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    compact.len() >= 3
        && compact
            .chars()
            .next()
            .is_some_and(|marker| matches!(marker, '-' | '*' | '_'))
        && compact
            .chars()
            .all(|character| character == compact.chars().next().unwrap())
}

fn is_markdown_table_start(lines: &[MarkdownSourceLine<'_>], index: usize) -> bool {
    index + 1 < lines.len()
        && lines[index].text.contains('|')
        && is_markdown_table_delimiter(lines[index + 1].text)
        && split_markdown_table_cells(lines[index].text).len()
            == split_markdown_table_cells(lines[index + 1].text).len()
}

fn is_markdown_table_delimiter(line: &str) -> bool {
    let cells = split_markdown_table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let core = cell.trim().trim_start_matches(':').trim_end_matches(':');
            core.len() >= 3 && core.chars().all(|character| character == '-')
        })
}

fn split_markdown_table_cells(line: &str) -> Vec<&str> {
    let trimmed = line.trim();
    let without_left = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let body = without_left.strip_suffix('|').unwrap_or(without_left);
    body.split('|').map(str::trim).collect()
}

fn markdown_leading_width(line: &str) -> usize {
    line.chars()
        .take_while(|character| character.is_whitespace())
        .map(|character| if character == '\t' { 4 } else { 1 })
        .sum()
}

fn markdown_list_item(line: &str) -> Option<(usize, &str, &str, bool)> {
    let content_start = line
        .char_indices()
        .find_map(|(index, character)| (!character.is_whitespace()).then_some(index))
        .unwrap_or(line.len());
    let trimmed = &line[content_start..];
    if let Some(content) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return Some((markdown_leading_width(line), "•", content, false));
    }
    let digits = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digits > 0 && trimmed[digits..].starts_with(". ") {
        return Some((
            markdown_leading_width(line),
            &trimmed[..=digits],
            &trimmed[digits + 2..],
            true,
        ));
    }
    None
}

fn render_markdown_block(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    source: &str,
    block: &MarkdownBlock,
    indent: &str,
    base_style: Style,
    width: usize,
) {
    match &block.kind {
        MarkdownBlockKind::Heading(level) => {
            let title_style = match level {
                1 => base_style.fg(ACCENT).bold().underlined(),
                2 => base_style.fg(ACCENT).bold(),
                _ => base_style.bold(),
            };
            let spans = vec![MarkdownSpan::plain(Span::raw(indent.to_string()))];
            push_wrapped_markdown_line(
                lines,
                hyperlinks,
                spans,
                inline_markdown_spans(&source[block.content_range.clone()], title_style),
                width,
            );
        }
        MarkdownBlockKind::Paragraph => {
            for raw in source[block.content_range.clone()].lines() {
                let spans = vec![MarkdownSpan::plain(Span::raw(indent.to_string()))];
                push_wrapped_markdown_line(
                    lines,
                    hyperlinks,
                    spans,
                    inline_markdown_spans(raw.trim(), base_style),
                    width,
                );
            }
        }
        MarkdownBlockKind::Code { language } => {
            if !language.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw(indent.to_string()),
                    Span::styled(language.clone(), Style::default().fg(DIM)),
                ]));
            }
            let prefix = format!("{indent}│ ");
            let available = width.saturating_sub(prefix.width()).max(1);
            let content = &source[block.content_range.clone()];
            let code_lines = if content.is_empty() {
                vec![""]
            } else {
                content.lines().collect::<Vec<_>>()
            };
            for raw in code_lines {
                for chunk in hard_wrap_preserving(raw, available) {
                    lines.push(Line::from(vec![
                        Span::styled(prefix.clone(), Style::default().fg(DIM)),
                        Span::styled(chunk, base_style.fg(Color::Yellow)),
                    ]));
                }
            }
        }
        MarkdownBlockKind::List => {
            for raw in source[block.range.clone()].lines() {
                if let Some((leading, marker, content, _ordered)) = markdown_list_item(raw) {
                    let prefix = format!("{indent}{}{marker} ", " ".repeat(leading.min(12)));
                    let spans = vec![MarkdownSpan::plain(Span::styled(
                        prefix,
                        Style::default().fg(ACCENT),
                    ))];
                    push_wrapped_markdown_line(
                        lines,
                        hyperlinks,
                        spans,
                        inline_markdown_spans(content, base_style),
                        width,
                    );
                } else {
                    let prefix = format!("{indent}  ");
                    let spans = vec![MarkdownSpan::plain(Span::raw(prefix))];
                    push_wrapped_markdown_line(
                        lines,
                        hyperlinks,
                        spans,
                        inline_markdown_spans(raw.trim(), base_style),
                        width,
                    );
                }
            }
        }
        MarkdownBlockKind::Quote => {
            for raw in source[block.range.clone()].lines() {
                let (depth, content) = markdown_quote_content(raw);
                let spans = vec![MarkdownSpan::plain(Span::styled(
                    format!("{indent}{}", "│ ".repeat(depth.max(1))),
                    Style::default().fg(ACCENT),
                ))];
                push_wrapped_markdown_line(
                    lines,
                    hyperlinks,
                    spans,
                    inline_markdown_spans(content, base_style.fg(Color::Gray).italic()),
                    width,
                );
            }
        }
        MarkdownBlockKind::Table => {
            render_markdown_table(
                lines,
                hyperlinks,
                &source[block.range.clone()],
                indent,
                base_style,
                width,
            );
        }
        MarkdownBlockKind::Rule => {
            let available = width.saturating_sub(indent.width());
            lines.push(Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled("─".repeat(available), Style::default().fg(DIM)),
            ]));
        }
    }
}

fn markdown_quote_content(mut line: &str) -> (usize, &str) {
    line = line.trim_start();
    let mut depth = 0;
    while let Some(rest) = line.strip_prefix('>') {
        depth += 1;
        line = rest.strip_prefix(' ').unwrap_or(rest);
    }
    (depth, line)
}

#[derive(Clone, Copy)]
enum MarkdownTableAlignment {
    Left,
    Center,
    Right,
}

fn markdown_table_alignment(cell: &str) -> MarkdownTableAlignment {
    let trimmed = cell.trim();
    match (trimmed.starts_with(':'), trimmed.ends_with(':')) {
        (true, true) => MarkdownTableAlignment::Center,
        (false, true) => MarkdownTableAlignment::Right,
        _ => MarkdownTableAlignment::Left,
    }
}

fn render_markdown_table(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    source: &str,
    indent: &str,
    base_style: Style,
    width: usize,
) {
    let raw_rows = source.lines().collect::<Vec<_>>();
    if raw_rows.len() < 2 {
        return;
    }
    let header = split_markdown_table_cells(raw_rows[0]);
    let delimiter = split_markdown_table_cells(raw_rows[1]);
    let column_count = header.len();
    if column_count == 0 || delimiter.len() != column_count {
        return;
    }
    let alignments = delimiter
        .iter()
        .map(|cell| markdown_table_alignment(cell))
        .collect::<Vec<_>>();
    let mut rows = vec![header];
    rows.extend(
        raw_rows
            .iter()
            .skip(2)
            .map(|line| split_markdown_table_cells(line)),
    );
    for row in &mut rows {
        row.resize(column_count, "");
        row.truncate(column_count);
    }

    let overhead = column_count.saturating_mul(3).saturating_add(1);
    let available = width.saturating_sub(indent.width());
    let content_budget = available.saturating_sub(overhead);
    if content_budget < column_count {
        for raw in raw_rows {
            let spans = vec![MarkdownSpan::plain(Span::raw(indent.to_string()))];
            push_wrapped_markdown_line(
                lines,
                hyperlinks,
                spans,
                inline_markdown_spans(raw, base_style),
                width,
            );
        }
        return;
    }

    let preferred = (0..column_count)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].width())
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect::<Vec<_>>();
    let mut widths = vec![1; column_count];
    let mut remaining = content_budget - column_count;
    while remaining > 0 {
        let mut changed = false;
        for column in 0..column_count {
            if remaining == 0 {
                break;
            }
            if widths[column] < preferred[column] {
                widths[column] += 1;
                remaining -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    append_markdown_table_row(
        lines,
        hyperlinks,
        &rows[0],
        &widths,
        &alignments,
        indent,
        base_style.fg(ACCENT).bold(),
    );
    let separator = widths
        .iter()
        .map(|column_width| "─".repeat(column_width + 2))
        .collect::<Vec<_>>()
        .join("┼");
    lines.push(Line::from(Span::styled(
        format!("{indent}├{separator}┤"),
        Style::default().fg(DIM),
    )));
    for row in rows.iter().skip(1) {
        append_markdown_table_row(
            lines,
            hyperlinks,
            row,
            &widths,
            &alignments,
            indent,
            base_style,
        );
    }
}

fn append_markdown_table_row(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    cells: &[&str],
    widths: &[usize],
    alignments: &[MarkdownTableAlignment],
    indent: &str,
    style: Style,
) {
    let wrapped = cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| wrap_markdown_spans(inline_markdown_spans(cell, style), *width))
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    for row_index in 0..height {
        let mut spans = vec![
            Span::raw(indent.to_string()),
            Span::styled("│", Style::default().fg(DIM)),
        ];
        for (column, column_width) in widths.iter().enumerate() {
            let content = wrapped[column].get(row_index).cloned().unwrap_or_default();
            let content_width = content.spans.iter().map(Span::width).sum::<usize>();
            let padding = column_width.saturating_sub(content_width);
            let (left, right) = match alignments[column] {
                MarkdownTableAlignment::Left => (0, padding),
                MarkdownTableAlignment::Center => (padding / 2, padding - padding / 2),
                MarkdownTableAlignment::Right => (padding, 0),
            };
            spans.push(Span::raw(format!(" {}", " ".repeat(left))));
            let link_offset = spans.iter().map(Span::width).sum::<usize>();
            for link in content.hyperlinks {
                hyperlinks.push(TranscriptHyperlink {
                    line: lines.len(),
                    start: link_offset + link.start,
                    end: link_offset + link.end,
                    url: link.url,
                });
            }
            spans.extend(content.spans);
            spans.push(Span::raw(format!("{} ", " ".repeat(right))));
            spans.push(Span::styled("│", Style::default().fg(DIM)));
        }
        lines.push(Line::from(spans));
    }
}

#[derive(Clone)]
struct MarkdownSpan {
    span: Span<'static>,
    hyperlink: Option<String>,
}

impl MarkdownSpan {
    fn plain(span: Span<'static>) -> Self {
        Self {
            span,
            hyperlink: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LineHyperlink {
    start: usize,
    end: usize,
    url: String,
}

#[derive(Clone, Default)]
struct WrappedMarkdownLine {
    spans: Vec<Span<'static>>,
    hyperlinks: Vec<LineHyperlink>,
}

fn push_wrapped_markdown_line(
    lines: &mut Vec<Line<'static>>,
    hyperlinks: &mut Vec<TranscriptHyperlink>,
    mut spans: Vec<MarkdownSpan>,
    content_spans: Vec<MarkdownSpan>,
    width: usize,
) {
    spans.extend(content_spans);
    let mut indent = Vec::new();
    let mut content = Vec::new();
    let mut reading_indent = true;

    for markdown_span in spans {
        let style = markdown_span.span.style;
        let text = markdown_span.span.content.into_owned();
        if reading_indent {
            let content_start = text
                .char_indices()
                .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index));
            match content_start {
                Some(index) => {
                    if index > 0 {
                        indent.push(MarkdownSpan {
                            span: Span::styled(text[..index].to_string(), style),
                            hyperlink: markdown_span.hyperlink.clone(),
                        });
                    }
                    content.push(MarkdownSpan {
                        span: Span::styled(text[index..].to_string(), style),
                        hyperlink: markdown_span.hyperlink,
                    });
                    reading_indent = false;
                }
                None => indent.push(MarkdownSpan {
                    span: Span::styled(text, style),
                    hyperlink: markdown_span.hyperlink,
                }),
            }
        } else {
            content.push(MarkdownSpan {
                span: Span::styled(text, style),
                hyperlink: markdown_span.hyperlink,
            });
        }
    }

    let indent_width = indent.iter().map(|span| span.span.width()).sum::<usize>();
    let available = width.saturating_sub(indent_width).max(1);
    let indent_spans = indent.into_iter().map(|span| span.span).collect::<Vec<_>>();
    for wrapped in wrap_markdown_spans(content, available) {
        let line = lines.len();
        hyperlinks.extend(
            wrapped
                .hyperlinks
                .into_iter()
                .map(|link| TranscriptHyperlink {
                    line,
                    start: indent_width + link.start,
                    end: indent_width + link.end,
                    url: link.url,
                }),
        );
        let mut row = indent_spans.clone();
        row.extend(wrapped.spans);
        lines.push(Line::from(row));
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
    wrap_markdown_spans(spans.into_iter().map(MarkdownSpan::plain).collect(), width)
        .into_iter()
        .map(|line| line.spans)
        .collect()
}

fn wrap_markdown_spans(spans: Vec<MarkdownSpan>, width: usize) -> Vec<WrappedMarkdownLine> {
    let width = width.max(1);
    let chars = spans
        .into_iter()
        .flat_map(|markdown_span| {
            let style = markdown_span.span.style;
            let hyperlink = markdown_span.hyperlink;
            markdown_span
                .span
                .content
                .into_owned()
                .chars()
                .map(move |ch| (ch, style, hyperlink.clone()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    if chars.is_empty() {
        return vec![WrappedMarkdownLine::default()];
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
        let mut line_hyperlinks = Vec::new();
        let mut current_hyperlink: Option<(String, usize)> = None;
        let mut column = 0;
        for (ch, style, hyperlink) in &chars[start..end] {
            if let Some(previous) = chunk.last_mut().filter(|span| span.style == *style) {
                previous.content.to_mut().push(*ch);
            } else {
                chunk.push(Span::styled(ch.to_string(), *style));
            }
            if current_hyperlink.as_ref().map(|(url, _)| url) != hyperlink.as_ref() {
                if let Some((url, link_start)) = current_hyperlink.take() {
                    line_hyperlinks.push(LineHyperlink {
                        start: link_start,
                        end: column,
                        url,
                    });
                }
                current_hyperlink = hyperlink.clone().map(|url| (url, column));
            }
            column += ch.width().unwrap_or(0);
        }
        if let Some((url, link_start)) = current_hyperlink {
            line_hyperlinks.push(LineHyperlink {
                start: link_start,
                end: column,
                url,
            });
        }
        wrapped.push(WrappedMarkdownLine {
            spans: chunk,
            hyperlinks: line_hyperlinks,
        });
        start = next;
    }
    wrapped
}

#[cfg(test)]
fn inline_spans(input: &str, base: Style) -> Vec<Span<'static>> {
    inline_markdown_spans(input, base)
        .into_iter()
        .map(|span| span.span)
        .collect()
}

fn inline_markdown_spans(input: &str, base: Style) -> Vec<MarkdownSpan> {
    #[derive(Clone, Copy)]
    enum Marker {
        BoldItalic,
        Bold,
        Italic(char),
        Strike,
        Code,
        Link,
    }

    let mut spans = Vec::new();
    let mut rest = input;
    while !rest.is_empty() {
        let next = [
            rest.find("***").map(|index| (index, Marker::BoldItalic)),
            rest.find("**").map(|index| (index, Marker::Bold)),
            rest.find("~~").map(|index| (index, Marker::Strike)),
            rest.find('`').map(|index| (index, Marker::Code)),
            rest.find('[').map(|index| (index, Marker::Link)),
            rest.find('*').map(|index| (index, Marker::Italic('*'))),
            rest.find('_').map(|index| (index, Marker::Italic('_'))),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(index, _)| *index);
        let Some((index, marker)) = next else {
            spans.push(MarkdownSpan::plain(Span::styled(rest.to_string(), base)));
            break;
        };
        if index > 0 {
            spans.push(MarkdownSpan::plain(Span::styled(
                rest[..index].to_string(),
                base,
            )));
        }
        match marker {
            Marker::BoldItalic
            | Marker::Bold
            | Marker::Italic(_)
            | Marker::Strike
            | Marker::Code => {
                let delimiter = match marker {
                    Marker::BoldItalic => "***",
                    Marker::Bold => "**",
                    Marker::Italic('*') => "*",
                    Marker::Italic('_') => "_",
                    Marker::Strike => "~~",
                    Marker::Code => "`",
                    Marker::Italic(_) | Marker::Link => unreachable!(),
                };
                let after = &rest[index + delimiter.len()..];
                if let Marker::Italic(character) = marker {
                    if !is_inline_italic_open(rest, index, character) {
                        spans.push(MarkdownSpan::plain(Span::styled(
                            delimiter.to_string(),
                            base,
                        )));
                        rest = after;
                        continue;
                    }
                }
                let end = match marker {
                    Marker::Italic(character) => find_inline_italic_close(after, character),
                    _ => after.find(delimiter),
                };
                if let Some(end) = end {
                    let style = match marker {
                        Marker::BoldItalic => base.bold().italic(),
                        Marker::Bold => base.bold(),
                        Marker::Italic(_) => base.italic(),
                        Marker::Strike => base.crossed_out(),
                        Marker::Code => base.fg(Color::Yellow),
                        Marker::Link => unreachable!(),
                    };
                    if matches!(marker, Marker::Code) {
                        spans.push(MarkdownSpan::plain(Span::styled(
                            after[..end].to_string(),
                            style,
                        )));
                    } else {
                        spans.extend(inline_markdown_spans(&after[..end], style));
                    }
                    rest = &after[end + delimiter.len()..];
                } else {
                    spans.push(MarkdownSpan::plain(Span::styled(
                        delimiter.to_string(),
                        base,
                    )));
                    rest = after;
                }
            }
            Marker::Link => {
                let after_open = &rest[index + 1..];
                if let Some(label_end) = after_open.find("](") {
                    let after_url_open = &after_open[label_end + 2..];
                    if let Some(url_end) = after_url_open.find(')') {
                        let hyperlink = safe_terminal_url(&after_url_open[..url_end]);
                        let mut label = inline_markdown_spans(
                            &after_open[..label_end],
                            base.fg(Color::Blue).underlined(),
                        );
                        for span in &mut label {
                            span.hyperlink = hyperlink.clone();
                        }
                        spans.extend(label);
                        rest = &after_url_open[url_end + 1..];
                        continue;
                    }
                }
                spans.push(MarkdownSpan::plain(Span::styled("[", base)));
                rest = after_open;
            }
        }
    }
    spans
}

fn safe_terminal_url(url: &str) -> Option<String> {
    let web_scheme = url.starts_with("https://") || url.starts_with("http://");
    (web_scheme
        && !url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace()))
    .then(|| url.to_string())
}

fn is_inline_italic_open(input: &str, index: usize, marker: char) -> bool {
    let after = &input[index + marker.len_utf8()..];
    if after.chars().next().is_none_or(char::is_whitespace) {
        return false;
    }
    marker != '_'
        || input[..index]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphanumeric())
}

fn find_inline_italic_close(input: &str, marker: char) -> Option<usize> {
    input.match_indices(marker).find_map(|(index, _)| {
        let before = input[..index].chars().next_back()?;
        if before.is_whitespace() {
            return None;
        }
        let after = &input[index + marker.len_utf8()..];
        if marker == '_' && after.chars().next().is_some_and(char::is_alphanumeric) {
            return None;
        }
        Some(index)
    })
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
    let shortcut_hint = if let Some(feedback) = &state.copy_feedback {
        feedback.clone()
    } else if let Some(copy_mode) = &state.copy_mode {
        copy_mode_hint(copy_mode, area.width)
    } else if !state.queued_turns.is_empty() {
        format!("{} queued · Ctrl+C remove latest", state.queued_turns.len())
    } else {
        composer_shortcut_hint(area.width).into()
    };
    let hint_style = if state.copy_feedback.is_some()
        || state.copy_mode.is_some()
        || !state.queued_turns.is_empty()
    {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };
    let block = Block::default()
        .title(Line::styled(format!(" {shortcut_hint} "), hint_style))
        .title_alignment(Alignment::Right)
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(if state.copy_mode.is_some() {
            ACCENT
        } else {
            DIM
        }))
        .padding(Padding::new(2, 2, 1, 1))
        .style(Style::default().bg(COMPOSER_BACKGROUND));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let queued = queued_turn_lines(&state.queued_turns, inner.width as usize);
    let queued_height = (queued.len() as u16).min(inner.height);
    if queued_height > 0 {
        frame.render_widget(
            Paragraph::new(queued),
            Rect::new(inner.x, inner.y, inner.width, queued_height),
        );
    }
    let attachments = attachment_labels(&state.image_attachments);
    let attachment_height =
        (attachments.len() as u16).min(inner.height.saturating_sub(queued_height));
    if attachment_height > 0 {
        let lines = attachments
            .into_iter()
            .map(|label| Line::styled(label, Style::default().fg(DIM)))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(
                inner.x,
                inner.y.saturating_add(queued_height),
                inner.width,
                attachment_height,
            ),
        );
    }
    let suggestions_y = inner
        .y
        .saturating_add(queued_height)
        .saturating_add(attachment_height);
    let suggestion_height = (suggestions.len() as u16).min(
        inner
            .height
            .saturating_sub(queued_height)
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
    let content_height = queued_height
        .saturating_add(attachment_height)
        .saturating_add(suggestion_height);
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
            Span::styled(
                if state.turn_id.is_some() || state.turn_started_at.is_some() {
                    "Queue next request…"
                } else {
                    "Ask Codex…"
                },
                Style::default().fg(DIM),
            ),
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
    if state.popup.is_none() && state.copy_mode.is_none() && input_area.height > 0 {
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

fn copy_mode_hint(copy_mode: &CopyMode, width: u16) -> String {
    match copy_mode {
        CopyMode::Answers { .. } => match width {
            72.. => "COPY · ANSWERS  j/k move · Enter blocks · y copy · Esc close".into(),
            48.. => "COPY · ANSWERS  j/k · Enter blocks · y copy · Esc".into(),
            30.. => "COPY · ANSWERS  j/k · Enter · y".into(),
            _ => "COPY · ANSWERS".into(),
        },
        CopyMode::Markdown { selected, .. } => {
            let count = selected.len();
            match width {
                72.. => format!(
                    "COPY · MARKDOWN  j/k move · Enter select · y copy ({count}) · Esc back"
                ),
                48.. => {
                    format!("COPY · MARKDOWN  j/k · Enter select · y ({count}) · Esc")
                }
                30.. => format!("COPY · MARKDOWN  j/k · Enter · y ({count})"),
                _ => format!("COPY · MARKDOWN ({count})"),
            }
        }
    }
}

fn composer_shortcut_hint(width: u16) -> &'static str {
    match width {
        100.. => {
            "Tab mode · Ctrl+Y copy · Ctrl+P/N history · Ctrl+K/J scroll · Ctrl+O expand · Ctrl+G end"
        }
        80.. => "Tab mode · Ctrl+Y copy · Ctrl+K/J scroll · Ctrl+O expand · Ctrl+G end",
        60.. => "Tab mode · Ctrl+Y copy · Ctrl+O expand · Ctrl+G end",
        42.. => "Ctrl+Y copy · Ctrl+G end",
        26.. => "Ctrl+Y copy",
        _ => "Tab mode",
    }
}

fn attachment_row_count(count: usize) -> u16 {
    count.min(3) as u16
}

fn queued_turn_row_count(count: usize) -> u16 {
    count.min(3) as u16
}

fn queued_turn_lines(
    queued: &std::collections::VecDeque<QueuedTurn>,
    width: usize,
) -> Vec<Line<'static>> {
    let total = queued.len();
    let visible = total.min(3);
    let message_rows = if total > 3 { visible - 1 } else { visible };
    let mut lines = queued
        .iter()
        .take(message_rows)
        .enumerate()
        .map(|(index, queued)| {
            let label = if total == 1 {
                "Next".to_string()
            } else {
                format!("Next {}", index + 1)
            };
            let preview = queued_turn_preview(queued);
            let prefix = format!("{label} · ");
            Line::from(vec![
                Span::styled(prefix.clone(), Style::default().fg(ACCENT).bold()),
                Span::styled(
                    truncate(&preview, width.saturating_sub(prefix.width())),
                    Style::default().fg(DIM),
                ),
            ])
        })
        .collect::<Vec<_>>();
    if total > 3 {
        lines.push(Line::from(Span::styled(
            format!("… +{} more queued", total - message_rows),
            Style::default().fg(DIM),
        )));
    }
    lines
}

fn queued_turn_preview(queued: &QueuedTurn) -> String {
    let text = queued.text.split_whitespace().collect::<Vec<_>>().join(" ");
    match (text.is_empty(), queued.images.len()) {
        (false, 0) => text,
        (false, count) => format!("{text} · {count} image(s)"),
        (true, count) => format!("{count} image(s)"),
    }
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
        Popup::TrustDirectory(prompt) => {
            let detail = trust_directory_detail(prompt);
            let option_count = if prompt.saving { 1 } else { 2 };
            (visual_line_count(&detail, width) as u16 + option_count + 3).clamp(7, 20)
        }
        Popup::Approval(approval) => {
            if approval.entering_feedback {
                let input_width = width.saturating_sub(6).max(1) as usize;
                let lines = layout_composer(
                    &approval.feedback.text,
                    approval.feedback.cursor,
                    input_width,
                )
                .lines
                .len()
                .clamp(1, 5) as u16;
                return lines + 4;
            }
            let option_count = approval_options(approval).len();
            let body_width = width.saturating_sub(4).max(1) as usize;
            let body_lines = approval_body_lines(approval, body_width).len() as u16;
            let maximum = if approval.expanded { 40 } else { 18 };
            (body_lines + option_count as u16 + 4).clamp(8, maximum)
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
            (question_lines + body_lines + 4).clamp(6, 18)
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
        Popup::Resume {
            selected,
            loading,
            scope,
        } => {
            let entries = if loading {
                vec!["Loading conversations…".to_string()]
            } else if state.threads.is_empty() {
                vec!["No conversations found".to_string()]
            } else {
                state
                    .threads
                    .iter()
                    .map(|thread| {
                        resume_entry_text(thread, area.width.saturating_sub(6) as usize, scope)
                    })
                    .collect()
            };
            let title = match scope {
                ResumeScope::CurrentDirectory => "Resume · current · Tab: all",
                ResumeScope::AllDirectories => "Resume · all · Tab: current",
            };
            draw_list_panel(frame, title, entries, selected, 0, area);
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
        Popup::TrustDirectory(prompt) => {
            let options = if prompt.saving {
                &["Saving trust…"][..]
            } else {
                &["Yes, continue", "No, quit"][..]
            };
            draw_action_panel(
                frame,
                "Trust directory",
                &trust_directory_detail(&prompt),
                options,
                prompt.selected.min(options.len().saturating_sub(1)),
                1,
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

fn trust_directory_detail(prompt: &TrustDirectoryPrompt) -> String {
    let mut detail = format!(
        "You are in {}\n\nDo you trust the contents of this directory? Trusting it allows project-local config, hooks, and exec policies to load.",
        prompt.cwd
    );
    if prompt.cwd != prompt.trust_target {
        detail.push_str(&format!(
            "\n\nTrust applies to the project root: {}",
            prompt.trust_target
        ));
    }
    if let Some(error) = prompt.error.as_deref() {
        detail.push_str(&format!("\n\nFailed to save trust: {error}"));
    }
    detail
}

fn approval_options(approval: &crate::model::Approval) -> Vec<String> {
    if matches!(approval.kind, ApprovalKind::Unsupported) {
        return vec!["Decline".to_string()];
    }
    let persistent = approval
        .params
        .get("proposedExecpolicyAmendment")
        .and_then(serde_json::Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .map(serde_json::Value::as_str)
                .collect::<Option<Vec<_>>>()
        })
        .filter(|parts| !parts.is_empty())
        .map(|parts| format!("Allow commands starting with `{}`", parts.join(" ")))
        .unwrap_or_else(|| "Allow for this session".to_string());
    vec![
        "Allow once".to_string(),
        persistent,
        "Deny".to_string(),
        "Deny and tell Magdex what to do differently".to_string(),
        "Deny and cancel turn".to_string(),
    ]
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
    if approval.entering_feedback {
        draw_approval_feedback_panel(frame, approval, area);
        return;
    }
    let options = approval_options(approval);
    let title = if approval.expanded {
        format!("{} · Ctrl+K/J scroll · Ctrl+O collapse", approval.title)
    } else {
        approval.title.clone()
    };
    let block = panel_block(&title, 1);
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

    let body = approval_body_lines(approval, chunks[0].width.max(1) as usize);
    if approval.expanded {
        let max_scroll = body.len().saturating_sub(chunks[0].height as usize);
        frame.render_widget(
            Paragraph::new(body).scroll((approval.scroll.min(max_scroll) as u16, 0)),
            chunks[0],
        );
    } else {
        let body = collapse_approval_body(body, chunks[0].height as usize, false);
        frame.render_widget(Paragraph::new(body), chunks[0]);
    }

    frame.render_widget(
        Paragraph::new("─".repeat(chunks[1].width as usize)).style(Style::default().fg(DIM)),
        chunks[1],
    );
    let entries = options
        .iter()
        .map(|option| ListItem::new(option.as_str()))
        .collect::<Vec<_>>();
    let list = List::new(entries)
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).bold());
    let mut list_state = ListState::default().with_selected(Some(approval.selected));
    frame.render_stateful_widget(list, chunks[2], &mut list_state);
}

fn update_approval_scroll_bounds(approval: &mut crate::model::Approval, area: Rect) {
    if !approval.expanded || approval.entering_feedback {
        approval.scroll = 0;
        approval.max_scroll = 0;
        return;
    }
    let option_count = approval_options(approval).len() as u16;
    let block = panel_block(&approval.title, 1);
    let inner = block.inner(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(option_count),
        ])
        .split(inner);
    let body_lines = approval_body_lines(approval, chunks[0].width.max(1) as usize).len();
    approval.max_scroll = body_lines.saturating_sub(chunks[0].height as usize);
    approval.scroll = approval.scroll.min(approval.max_scroll);
}

fn draw_approval_feedback_panel(frame: &mut Frame, approval: &crate::model::Approval, area: Rect) {
    let block = panel_block("Tell Magdex what to do differently", 1);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let input_width = chunks[0].width.saturating_sub(2).max(1) as usize;
    let layout = layout_composer(
        &approval.feedback.text,
        approval.feedback.cursor,
        input_width,
    );
    let visible_lines = chunks[0].height.max(1) as usize;
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
                if line.is_empty() && approval.feedback.text.is_empty() {
                    Span::styled("Describe a better approach…", Style::default().fg(DIM))
                } else {
                    Span::raw(line.clone())
                },
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).scroll((vertical_scroll as u16, 0)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new("Enter send · Shift+Enter newline · Esc back")
            .style(Style::default().fg(DIM)),
        chunks[1],
    );
    let cursor_x = chunks[0]
        .x
        .saturating_add(2)
        .saturating_add(layout.cursor_col.min(input_width) as u16);
    let cursor_y = chunks[0]
        .y
        .saturating_add(layout.cursor_row.saturating_sub(vertical_scroll) as u16);
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

fn approval_body_lines(approval: &crate::model::Approval, width: usize) -> Vec<Line<'static>> {
    let command = matches!(approval.kind, ApprovalKind::Command)
        || matches!(approval.kind, ApprovalKind::Legacy) && approval.title == "Run command?";
    if !command {
        let mut lines = Vec::new();
        for detail in approval.detail.lines() {
            push_wrapped_line(&mut lines, vec![Span::raw(detail.to_string())], width);
        }
        return lines;
    }

    let mut lines = Vec::new();
    if let Some(reason) = approval.reason.as_deref() {
        push_wrapped_line(
            &mut lines,
            vec![
                Span::styled("Reason: ", Style::default().fg(DIM)),
                Span::styled(reason.to_string(), Style::default().italic()),
            ],
            width,
        );
        lines.push(Line::default());
    }
    for command in approval.detail.lines() {
        let mut spans = vec![Span::styled(
            "$ ",
            Style::default().fg(Color::Yellow).bold(),
        )];
        spans.extend(shell_command_spans(command));
        push_wrapped_line(&mut lines, spans, width);
    }
    lines
}

fn collapse_approval_body(
    lines: Vec<Line<'static>>,
    available: usize,
    expanded: bool,
) -> Vec<Line<'static>> {
    if expanded || lines.len() <= available || available < 3 {
        return lines;
    }
    let leading = available.saturating_sub(2);
    let hidden = lines.len().saturating_sub(leading + 1);
    let mut visible = lines.iter().take(leading).cloned().collect::<Vec<_>>();
    visible.push(Line::from(Span::styled(
        format!("… +{hidden} lines · Ctrl+O to expand"),
        Style::default().fg(DIM),
    )));
    if let Some(last) = lines.last() {
        visible.push(last.clone());
    }
    visible
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
    let block = panel_block(&title, 1);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let question_height = visual_line_count(&question.question, area.width) as u16;

    if request.is_editing() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(question_height),
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
            "Enter answer · Shift+Enter newline · Esc back"
        } else {
            "Enter answer · Shift+Enter newline · Esc cancel"
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
            Constraint::Length(question_height),
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

fn truncate_start(value: &str, max_width: usize) -> String {
    if value.width() <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut suffix = Vec::new();
    let mut width = 1;
    for ch in value.chars().rev() {
        let next = ch.width().unwrap_or(0);
        if width + next > max_width {
            break;
        }
        width += next;
        suffix.push(ch);
    }
    suffix.reverse();
    format!("…{}", suffix.into_iter().collect::<String>())
}

fn resume_entry_parts(
    thread: &ThreadSummary,
    max_width: usize,
    scope: ResumeScope,
) -> (String, String) {
    let age = relative_time(thread.updated_at);
    let metadata = match scope {
        ResumeScope::CurrentDirectory => format!("  · {age}"),
        ResumeScope::AllDirectories => {
            let path = display_path(&thread.cwd, std::env::var("HOME").ok().as_deref());
            let path_width = (max_width / 3)
                .clamp(8, 30)
                .min(max_width.saturating_sub(age.width()).saturating_sub(12));
            format!("  · {}  · {age}", truncate_start(&path, path_width))
        }
    };
    let title_width = max_width.saturating_sub(metadata.width()).max(1);
    (truncate(&thread.title, title_width), metadata)
}

fn resume_entry_text(thread: &ThreadSummary, max_width: usize, scope: ResumeScope) -> String {
    let (title, metadata) = resume_entry_parts(thread, max_width, scope);
    format!("{title}{metadata}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_shows_active_context_and_mode_in_separator() {
        let backend = ratatui::backend::TestBackend::new(80, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/home/user/magdex".into(), true);
        state.project = "~/magdex".into();
        state.model = Some("gpt-test".into());
        state.effort = Some("high".into());
        state.context_usage = Some(ContextUsage {
            input_tokens: 24_763,
            context_window: Some(258_400),
        });
        state.context_compactions = 2;

        terminal
            .draw(|frame| draw_header(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = |y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).trim().is_empty());
        assert!(row(1).contains("gpt-test · high"));
        assert!(!row(1).contains("default"));
        assert!(row(1).contains("24.8k (10%)"));
        assert!(!row(1).contains("context"));
        assert!(!row(1).contains("compactions"));
        assert!(!row(1).contains('│'));
        assert!(row(1).trim_end().ends_with("24.8k (10%)"));
        assert!(!row(1).contains("tokens 2.3m"));
        assert!(row(2).trim().is_empty());
        assert!(row(3).trim_start().starts_with("Default"));
        assert!(row(3).trim_end().ends_with("~/magdex"));
        assert!(row(3).find("Default").unwrap() < row(3).find("~/magdex").unwrap());
        let mode_cell = (0..80).find(|x| buffer[(*x, 3)].symbol() == "D").unwrap();
        let project_cell = (0..80).find(|x| buffer[(*x, 3)].symbol() == "~").unwrap();
        assert_eq!(buffer[(mode_cell, 3)].fg, DIM);
        assert_eq!(buffer[(project_cell, 3)].fg, DIM);
        assert_eq!(buffer[(40, 3)].fg, Color::White);
    }

    #[test]
    fn header_keeps_essential_metrics_at_narrow_width() {
        let backend = ratatui::backend::TestBackend::new(41, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/home/user/magdex".into(), true);
        state.model = Some("gpt-test".into());
        state.effort = Some("high".into());
        state.context_usage = Some(ContextUsage {
            input_tokens: 24_763,
            context_window: Some(258_400),
        });
        state.context_compactions = 2;
        state.quota_usage = QuotaUsage {
            five_hour: Some(crate::model::QuotaWindow {
                used_percent: 19,
                window_minutes: 300,
                resets_at: None,
            }),
            weekly: Some(crate::model::QuotaWindow {
                used_percent: 74,
                window_minutes: 10_080,
                resets_at: None,
            }),
        };

        terminal
            .draw(|frame| draw_header(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..41).map(|x| buffer[(x, 1)].symbol()).collect::<String>();
        let metadata = row.find("gpt-test · high").unwrap();
        let quota = row.find("5h 81% · week 26%").unwrap();
        assert!(quota - (metadata + "gpt-test · high".width()) >= HEADER_GROUP_GAP);
        assert!(!row.contains("24.8k"));
        assert!(!row.contains("quota"));
        assert!(!row.contains("context"));
        assert!(!row.contains("compactions"));
        assert!(!row.contains('│'));
    }

    #[test]
    fn header_context_handles_missing_window_or_usage() {
        let usage = ContextUsage {
            input_tokens: 24_763,
            context_window: None,
        };

        assert_eq!(header_context_variants(Some(&usage)), vec!["24.8k"]);
        assert!(header_context_variants(None).is_empty());
    }

    #[test]
    fn header_places_remaining_quota_between_metadata_and_context() {
        let backend = ratatui::backend::TestBackend::new(80, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/home/user/magdex".into(), true);
        state.model = Some("gpt-test".into());
        state.effort = Some("high".into());
        state.context_usage = Some(ContextUsage {
            input_tokens: 24_763,
            context_window: Some(258_400),
        });
        state.context_compactions = 2;
        state.quota_usage = QuotaUsage {
            five_hour: Some(crate::model::QuotaWindow {
                used_percent: 19,
                window_minutes: 300,
                resets_at: None,
            }),
            weekly: Some(crate::model::QuotaWindow {
                used_percent: 74,
                window_minutes: 10_080,
                resets_at: None,
            }),
        };

        terminal
            .draw(|frame| draw_header(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..80).map(|x| buffer[(x, 1)].symbol()).collect::<String>();
        let metadata = row.find("gpt-test · high").unwrap();
        let quota = row.find("5h 81% · week 26%").unwrap();
        let context = row.find("24.8k (10%)").unwrap();
        assert!(metadata < quota && quota < context);
        assert!(quota - (metadata + "gpt-test · high".width()) >= HEADER_GROUP_GAP);
        assert!(context - (quota + "5h 81% · week 26%".width()) >= HEADER_GROUP_GAP);
        let quota_cell = (0..79)
            .find(|x| buffer[(*x, 1)].symbol() == "5" && buffer[(*x + 1, 1)].symbol() == "h")
            .unwrap();
        assert!(
            (quota_cell..quota_cell + "5h 81% · week 26%".width() as u16)
                .all(|x| buffer[(x, 1)].fg == DIM)
        );
    }

    #[test]
    fn header_capitalizes_mode_names() {
        assert_eq!(display_mode("default"), "Default");
        assert_eq!(display_mode("plan"), "Plan");
    }

    #[test]
    fn header_hides_project_before_separator_titles_touch() {
        let backend = ratatui::backend::TestBackend::new(20, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/home/user/magdex".into(), true);
        state.project = "~/long-project".into();

        terminal
            .draw(|frame| draw_header(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..20).map(|x| buffer[(x, 3)].symbol()).collect::<String>();
        assert!(row.contains("Default"));
        assert!(!row.contains("long-project"));
    }

    #[test]
    fn context_compaction_renders_as_a_separate_transcript_block() {
        let block = TranscriptBlock::new(
            BlockKind::Compaction,
            "Context compacted",
            "Earlier conversation was summarized to free context · 2 total",
        );
        let mut lines = Vec::new();

        append_block(&mut lines, &block, 80, 80, "/tmp/project", false);

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Context compacted"));
        assert!(rendered.contains("summarized to free context · 2 total"));
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
    fn expanded_command_remains_open_when_no_longer_latest() {
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
        assert!(rendered.contains("Ctrl+O"));
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
        assert!(!old_rendered.contains("… +"));
        assert!(!old_rendered.contains("Ctrl+O"));
        assert_eq!(old_rendered.chars().filter(|ch| *ch == 'x').count(), 200);
    }

    #[test]
    fn long_commands_keep_their_start_and_end_without_flooding_transcript() {
        let command = format!(
            "python3 -c {} больше",
            (0..24)
                .map(|index| format!("argument-{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut block = TranscriptBlock::new(BlockKind::Command, command, "");
        block.action_status = Some(ActionStatus::Completed);
        block.exit_code = Some(0);
        let mut lines = Vec::new();

        append_command(&mut lines, &block, 32, true);

        assert_eq!(lines.len(), COLLAPSED_COMMAND_ROWS + 1);
        assert!(lines.iter().all(|line| line.width() <= 32));
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Ran python3"));
        assert!(rendered.contains("Ctrl+O"));
        assert!(rendered.contains("больше"));

        block.expanded = true;
        let mut expanded = Vec::new();
        append_command(&mut expanded, &block, 32, true);
        assert!(expanded.len() > lines.len());
        assert!(!expanded
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content.contains("Ctrl+O")));
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
    fn markdown_web_links_keep_safe_terminal_targets() {
        let spans = inline_markdown_spans(
            "[first link](https://example.com/a) and [second](http://example.org)",
            Style::default(),
        );
        let targets = spans
            .iter()
            .filter_map(|span| span.hyperlink.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(targets, ["https://example.com/a", "http://example.org"]);

        for source in [
            "[file](/tmp/file)",
            "[mail](mailto:user@example.com)",
            "[bad](https://example.com/unsafe url)",
            "[bad](https://example.com/\u{1b}escape)",
        ] {
            assert!(inline_markdown_spans(source, Style::default())
                .iter()
                .all(|span| span.hyperlink.is_none()));
        }
    }

    #[test]
    fn markdown_link_ranges_survive_word_wrapping() {
        let wrapped = wrap_markdown_spans(
            inline_markdown_spans("[one two three](https://example.com)", Style::default()),
            5,
        );
        let rows = wrapped
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, ["one", "two", "three"]);
        assert!(wrapped.iter().all(|line| {
            line.hyperlinks
                == [LineHyperlink {
                    start: 0,
                    end: line.spans.iter().map(Span::width).sum(),
                    url: "https://example.com".into(),
                }]
        }));
    }

    #[test]
    fn markdown_renderer_places_link_ranges_on_transcript_lines() {
        let mut lines = Vec::new();
        let mut hyperlinks = Vec::new();

        append_markdown_with_hyperlinks(
            &mut lines,
            &mut hyperlinks,
            "before [one two](https://example.com) after",
            "  ",
            Style::default(),
            12,
        );

        assert_eq!(hyperlinks.len(), 2);
        assert_eq!(hyperlinks[0].url, "https://example.com");
        assert_eq!(hyperlinks[1].url, "https://example.com");
        for link in hyperlinks {
            let line = &lines[link.line];
            let rendered = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(rendered[link.start..link.end]
                .trim()
                .chars()
                .next()
                .is_some_and(char::is_alphabetic));
        }
    }

    #[test]
    fn terminal_overlay_collects_visible_link_cells() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 3));
        buffer.set_string(2, 1, "link", Style::default().blue().underlined());
        let overlay = terminal_hyperlink_overlay(
            &buffer,
            &[VisibleHyperlink {
                row: 1,
                start: 2,
                end: 6,
                url: "https://example.com".into(),
            }],
        );

        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay[0].url, "https://example.com");
        assert_eq!(
            overlay[0]
                .cells
                .iter()
                .map(|cell| cell.cell.symbol())
                .collect::<String>(),
            "link"
        );
    }

    #[test]
    fn markdown_parser_finds_the_supported_top_level_blocks() {
        let source = "# Title\n\nparagraph\ncontinued\n\n```rust\nfn main() {}\n```\n\n- one\n  - nested\n\n1. first\n2. second\n\n> quote\n> continued\n\n| Name | Score |\n| --- | ---: |\n| Ada | 10 |\n\n---";

        let blocks = parse_markdown_blocks(source);
        let kinds = blocks
            .iter()
            .map(|block| block.kind.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            kinds,
            [
                MarkdownBlockKind::Heading(1),
                MarkdownBlockKind::Paragraph,
                MarkdownBlockKind::Code {
                    language: "rust".into()
                },
                MarkdownBlockKind::List,
                MarkdownBlockKind::List,
                MarkdownBlockKind::Quote,
                MarkdownBlockKind::Table,
                MarkdownBlockKind::Rule,
            ]
        );
        let code = &blocks[2];
        assert_eq!(&source[code.content_range.clone()], "fn main() {}\n");
        assert_eq!(&source[code.range.clone()], "```rust\nfn main() {}\n```");
    }

    #[test]
    fn markdown_copy_ranges_skip_rules_and_strip_code_fences() {
        let source = "# Heading\n\nParagraph\n\n---\n\n```rust\nfn main() {}\n```";
        let copied = markdown_copy_ranges(source)
            .into_iter()
            .map(|range| &source[range])
            .collect::<Vec<_>>();

        assert_eq!(copied, ["# Heading", "Paragraph", "fn main() {}"]);
    }

    #[test]
    fn markdown_inline_styles_cover_emphasis_strike_code_and_links() {
        let spans = inline_spans(
            "**bold** *italic* _also_ ***both*** ~~gone~~ `code` [link](https://example.com)",
            Style::default(),
        );
        let span = |content: &str| spans.iter().find(|span| span.content == content).unwrap();

        assert!(span("bold").style.add_modifier.contains(Modifier::BOLD));
        assert!(span("italic").style.add_modifier.contains(Modifier::ITALIC));
        assert!(span("also").style.add_modifier.contains(Modifier::ITALIC));
        assert!(span("both").style.add_modifier.contains(Modifier::BOLD));
        assert!(span("both").style.add_modifier.contains(Modifier::ITALIC));
        assert!(span("gone")
            .style
            .add_modifier
            .contains(Modifier::CROSSED_OUT));
        assert_eq!(span("code").style.fg, Some(Color::Yellow));
        assert_eq!(span("link").style.fg, Some(Color::Blue));
        assert!(span("link")
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED));

        let identifiers = inline_spans("message_history_position = left * right", Style::default());
        assert_eq!(
            identifiers
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "message_history_position = left * right"
        );
    }

    #[test]
    fn markdown_renderer_distinguishes_blocks_and_fits_tables() {
        let source = "# Main\n\n## Secondary\n\n#### Detail\n\n- bullet\n  2. nested\n\n> quoted text\n\n| Name | Long value |\n| :--- | ---: |\n| Ada | wrapped table content |\n\n```rust\nlet число = 10;\n```\n\n---";
        let mut lines = Vec::new();

        append_markdown(&mut lines, source, "  ", Style::default(), 32);

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Main"));
        assert!(rendered.contains("Secondary"));
        assert!(rendered.contains("Detail"));
        assert!(!rendered.contains('#'));
        assert!(rendered.contains("• bullet"));
        assert!(rendered.contains("2. nested"));
        assert!(rendered.contains("│ quoted text"));
        assert!(rendered.contains("Name"));
        assert!(rendered.contains("Long"));
        assert!(rendered.contains("rust"));
        assert!(rendered.contains("let число = 10;"));
        assert!(lines.iter().all(|line| line.width() <= 32));
        let main = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "Main")
            .unwrap();
        assert_eq!(main.style.fg, Some(ACCENT));
        assert!(main.style.add_modifier.contains(Modifier::BOLD));
        assert!(main.style.add_modifier.contains(Modifier::UNDERLINED));
        let secondary = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "Secondary")
            .unwrap();
        assert_eq!(secondary.style.fg, Some(ACCENT));
        assert!(secondary.style.add_modifier.contains(Modifier::BOLD));
        assert!(!secondary.style.add_modifier.contains(Modifier::UNDERLINED));
        let detail = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "Detail")
            .unwrap();
        assert_eq!(detail.style.fg, None);
        assert!(detail.style.add_modifier.contains(Modifier::BOLD));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("table") && span.style.add_modifier.is_empty())
        }));
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
    fn growing_composer_keeps_transcript_pinned_to_bottom() {
        let backend = ratatui::backend::TestBackend::new(40, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![TranscriptBlock::new(
            BlockKind::Assistant,
            "Codex",
            (0..30)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )];
        state.mark_transcript_dirty();
        state.jump_to_bottom();

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let initial_height = state.transcript_viewport_height;
        let initial_scroll = state.scroll;

        state.composer.insert_str(&"long composer text ".repeat(12));
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        assert!(state.transcript_viewport_height < initial_height);
        assert!(state.scroll > initial_scroll);
        assert_eq!(state.scroll, state.transcript_max_scroll);
        assert!(state.at_bottom);
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
        let top = (0..40).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert!(top.contains("Ctrl+Y copy"));
    }

    #[test]
    fn composer_shortcuts_adapt_to_the_available_width() {
        assert!(composer_shortcut_hint(100).contains("Ctrl+P/N history"));
        assert!(composer_shortcut_hint(80).contains("Ctrl+Y copy"));
        assert!(!composer_shortcut_hint(80).contains("Ctrl+P/N"));
        assert_eq!(composer_shortcut_hint(40), "Ctrl+Y copy");
        assert_eq!(composer_shortcut_hint(20), "Tab mode");
    }

    #[test]
    fn copy_mode_keeps_the_composer_and_changes_only_its_help() {
        let backend = ratatui::backend::TestBackend::new(80, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.composer.replace("preserved draft".into());
        state.copy_mode = Some(CopyMode::Markdown {
            block_index: 0,
            markdown_index: 1,
            selected: vec![0, 2],
        });
        let layout = layout_composer(&state.composer.text, state.composer.cursor, 74);

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
        assert!(rendered.contains("COPY · MARKDOWN"));
        assert!(rendered.contains("Enter select"));
        assert!(rendered.contains("y copy (2)"));
        assert!(rendered.contains("preserved draft"));
        assert_eq!(state.composer.text, "preserved draft");
    }

    #[test]
    fn copy_mode_highlights_the_answer_cursor_and_marked_blocks() {
        let backend = ratatui::backend::TestBackend::new(48, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![TranscriptBlock::new(
            BlockKind::Assistant,
            "Codex",
            "# Heading\n\nParagraph\n\n```rust\ncode\n```",
        )];
        state.copy_mode = Some(CopyMode::Markdown {
            block_index: 0,
            markdown_index: 1,
            selected: vec![0],
        });

        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();

        assert_eq!(state.transcript_markdown_ranges[0].len(), 3);
        let buffer = terminal.backend().buffer();
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.bg == COPY_SELECTED_BACKGROUND));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.bg == COPY_CURSOR_BACKGROUND));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.bg == COPY_ANSWER_BACKGROUND));
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
    fn composer_shows_queued_turns_above_the_next_prompt() {
        let backend = ratatui::backend::TestBackend::new(48, 14);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.turn_id = Some("turn-1".into());
        state.queued_turns.push_back(QueuedTurn {
            text: "Review the tests after this finishes".into(),
            images: vec![],
        });
        state.queued_turns.push_back(QueuedTurn {
            text: "Then update the documentation".into(),
            images: vec![],
        });

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let rows = (0..14)
            .map(|y| {
                (0..48)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|row| row.contains("Next 1 · Review")));
        assert!(rows.iter().any(|row| row.contains("Next 2 · Then")));
        assert!(rows.iter().any(|row| row.contains("Queue next request…")));
        assert!(rows.iter().any(|row| row.contains("2 queued")));
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
            scope: ResumeScope::CurrentDirectory,
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
        assert!(rendered.contains("Recent conversations · current"));
        assert!(!rendered.contains("Newest conversations"));
        assert!(rendered.contains("Fix transcript width"));
        assert!(rendered.contains("Ctrl+C quit"));
        assert!(!rendered.contains("Esc quit"));
        assert!(!rendered.contains("Connecting to app-server"));
        assert!(!rendered.contains("Ask Codex"));
    }

    #[test]
    fn global_resume_entries_show_their_working_directory() {
        let thread = ThreadSummary {
            id: "thread-1".into(),
            title: "Fix a different project".into(),
            cwd: "/home/user/projects/another-project".into(),
            updated_at: 0,
        };

        let current = resume_entry_text(&thread, 72, ResumeScope::CurrentDirectory);
        let global = resume_entry_text(&thread, 72, ResumeScope::AllDirectories);

        assert!(!current.contains("another-project"));
        assert!(global.contains("another-project"));
        assert!(global.width() <= 72);
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
    fn structured_choices_follow_the_question_and_keep_bottom_padding() {
        let backend = ratatui::backend::TestBackend::new(100, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let request = UserInputRequest {
            id: serde_json::json!(1),
            questions: vec![crate::model::UserInputQuestion {
                id: "release".into(),
                header: "Release".into(),
                question: "How should the release be prepared?".into(),
                options: vec![
                    crate::model::UserInputOption {
                        label: "Full release".into(),
                        description: "Commit and tag".into(),
                    },
                    crate::model::UserInputOption {
                        label: "Prepare only".into(),
                        description: "No tag".into(),
                    },
                    crate::model::UserInputOption {
                        label: "Skip".into(),
                        description: "Do nothing".into(),
                    },
                ],
                allow_other: false,
                secret: false,
            }],
            current: 0,
            answers: vec![],
            selected: 0,
            input: Default::default(),
            entering_other: false,
        };

        terminal
            .draw(|frame| draw_user_input_panel(frame, &request, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = |y| {
            (0..100)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(row(2).contains("How should the release be prepared?"));
        assert!(row(3).contains("Full release"));
        assert!(row(5).contains("Skip"));
        assert!(row(6).contains("Enter answer"));
        assert!(row(7).trim().is_empty());
        assert_eq!(buffer[(0, 7)].bg, COMPOSER_BACKGROUND);
        assert_eq!(buffer[(99, 7)].bg, COMPOSER_BACKGROUND);
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
    fn trust_prompt_shows_directory_and_requires_explicit_confirmation() {
        let backend = ratatui::backend::TestBackend::new(70, 18);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/work/project/src".into(), true);
        state.popup = Some(Popup::TrustDirectory(TrustDirectoryPrompt {
            cwd: "/work/project/src".into(),
            trust_target: "/work/project".into(),
            selected: 0,
            saving: false,
            error: None,
        }));

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Trust directory"));
        assert!(rendered.contains("/work/project/src"));
        assert!(rendered.contains("project root: /work/project"));
        assert!(rendered.contains("Yes, continue"));
        assert!(rendered.contains("No, quit"));
    }

    #[test]
    fn trust_prompt_keeps_answers_close_and_has_bottom_padding() {
        let backend = ratatui::backend::TestBackend::new(160, 18);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/work/project".into(), true);
        state.popup = Some(Popup::TrustDirectory(TrustDirectoryPrompt {
            cwd: "/work/project".into(),
            trust_target: "/work/project".into(),
            selected: 0,
            saving: false,
            error: None,
        }));

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let buffer = terminal.backend().buffer();
        let row = |y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(row(14).contains("Do you trust"));
        assert!(row(15).contains("Yes, continue"));
        assert!(row(16).contains("No, quit"));
        assert!(row(17).trim().is_empty());
        assert_eq!(buffer[(0, 17)].bg, COMPOSER_BACKGROUND);
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
            reason: Some("Verify approval rendering".into()),
            params: serde_json::json!({}),
            selected: 0,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: false,
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
        assert!(rows
            .iter()
            .any(|row| row.contains("Reason: Verify approval rendering")));
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
    fn approval_collapses_long_commands_and_names_the_proposed_rule() {
        let approval = crate::model::Approval {
            id: serde_json::json!(1),
            kind: ApprovalKind::Command,
            title: "Run command?".into(),
            detail: format!("cargo test {} final-argument", "long-argument ".repeat(20)),
            reason: Some("Verify approval rendering".into()),
            params: serde_json::json!({"proposedExecpolicyAmendment": ["cargo", "test"]}),
            selected: 0,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: false,
        };

        let body = approval_body_lines(&approval, 24);
        let collapsed = collapse_approval_body(body.clone(), 6, false);
        let rendered = collapsed
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(collapsed.len(), 6);
        assert!(rendered.contains("Ctrl+O to expand"));
        assert!(rendered.contains("final-argument"));
        assert_eq!(collapse_approval_body(body.clone(), body.len(), true), body);
        assert_eq!(
            approval_options(&approval)[1],
            "Allow commands starting with `cargo test`"
        );
    }

    #[test]
    fn approval_feedback_replaces_the_options_with_an_editor() {
        let backend = ratatui::backend::TestBackend::new(64, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut approval = crate::model::Approval {
            id: serde_json::json!(1),
            kind: ApprovalKind::Command,
            title: "Run command?".into(),
            detail: "cargo test".into(),
            reason: None,
            params: serde_json::json!({}),
            selected: 3,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: true,
        };
        approval.feedback.insert_str("Run only the focused test");

        terminal
            .draw(|frame| draw_approval_panel(frame, &approval, frame.area()))
            .unwrap();

        let rows = (0..8)
            .map(|y| {
                (0..64)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rows
            .iter()
            .any(|row| row.contains("Tell Magdex what to do differently")));
        assert!(rows
            .iter()
            .any(|row| row.contains("› Run only the focused test")));
        assert!(rows.iter().any(|row| row.contains("Enter send")));
    }

    #[test]
    fn expanded_approval_scrolls_the_full_command_body() {
        let backend = ratatui::backend::TestBackend::new(64, 14);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let approval = crate::model::Approval {
            id: serde_json::json!(1),
            kind: ApprovalKind::Command,
            title: "Run command?".into(),
            detail: (0..18)
                .map(|index| format!("command-segment-{index:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
            reason: None,
            params: serde_json::json!({}),
            selected: 0,
            expanded: true,
            scroll: 10,
            max_scroll: 15,
            feedback: Default::default(),
            entering_feedback: false,
        };

        terminal
            .draw(|frame| draw_approval_panel(frame, &approval, frame.area()))
            .unwrap();

        let rendered = (0..14)
            .map(|y| {
                (0..64)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Ctrl+K/J scroll"));
        assert!(!rendered.contains("command-segment-00"));
        assert!(rendered.contains("command-segment-10"));
        assert!(rendered.contains("Allow once"));
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
