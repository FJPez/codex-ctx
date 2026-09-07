//! Renders a `ContextCard` inside a border. Layout arithmetic only.

use codex_context_profiler::InvalidationReason;
use codex_context_profiler::TokenCost;
use codex_protocol::num_format::format_with_separators;
use ratatui::prelude::*;
use ratatui::style::Stylize;

use crate::history_cell::CompositeHistoryCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::plain_lines;
use crate::history_cell::with_border_with_inner_width;
use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_to_width;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::style::StatusTone;
use crate::style::status_style;
use crate::width::display_width;

use super::card::ContextCard;
use super::card::Contributor;
use super::card::Row;
use super::card::Startup;
use super::card::TurnRow;
use super::card::Usage;

/// Longest share bar, in cells.
const BAR_CELLS: usize = 14;
/// Width of a right-aligned share column, sized for "100%".
const SHARE_WIDTH: usize = 4;
const SHARE_HEADING: &str = "share of context";
/// The marker for a turn with no prior anchor to measure against.
const NO_ANCHOR: &str = "\u{2014}";
/// Gap between two columns.
const GAP: usize = 2;

pub(crate) fn new_context_card_cell(card: ContextCard) -> CompositeHistoryCell {
    let command = PlainHistoryCell::new(vec!["/ctx".magenta().into()]);
    CompositeHistoryCell::new(vec![Box::new(command), Box::new(ContextCardCell(card))])
}

#[derive(Debug)]
struct ContextCardCell(ContextCard);

impl HistoryCell for ContextCardCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let available_inner_width = usize::from(width.saturating_sub(4));
        if available_inner_width == 0 {
            return Vec::new();
        }
        let lines = content_lines(&self.0, available_inner_width);
        let content_width = lines.iter().map(line_width).max().unwrap_or(0);
        let inner_width = content_width.min(available_inner_width).max(1);
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| truncate_line_to_width(line, inner_width))
            .collect();
        with_border_with_inner_width(lines, inner_width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }
}

/// Rows are laid out to fit `inner_width`; the ellipsis pass only catches the prose lines.
fn content_lines(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    card_lines(card, inner_width)
        .into_iter()
        .map(|line| truncate_line_with_ellipsis_if_overflow(line, inner_width))
        .collect()
}

fn card_lines(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    if let Some(reason) = card.invalidated.as_ref() {
        return invalidated_lines(reason);
    }
    if card.header.is_none() && !card.has_items && card.startup == Startup::Unavailable {
        return vec!["Context profiler has no data for this thread yet.".into()];
    }

    let blocks = vec![
        context_block(card),
        startup_block(card, inner_width),
        attributed_block(card, inner_width),
        pending_block(card),
        not_attributable_block(card, inner_width),
        contributors_block(card, inner_width),
        turns_block(card, inner_width),
        warnings_block(card),
    ];
    let mut lines: Vec<Line<'static>> = Vec::new();
    for block in blocks.into_iter().filter(|block| !block.is_empty()) {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.extend(block);
    }
    lines
}

fn invalidated_lines(reason: &InvalidationReason) -> Vec<Line<'static>> {
    let cause = match reason {
        InvalidationReason::EventsDropped { skipped } => {
            format!("{skipped} app-server events were dropped")
        }
        InvalidationReason::Compacted => "The context was compacted".to_string(),
        InvalidationReason::SequenceMismatch { .. } => "The item stream was incomplete".to_string(),
    };
    vec![
        Span::styled(
            "\u{26a0} Context breakdown incomplete",
            status_style(StatusTone::Attention),
        )
        .into(),
        cause.into(),
        "Attribution unavailable for the rest of this session".into(),
        "See /status for current usage".dim().into(),
    ]
}

fn context_block(card: &ContextCard) -> Vec<Line<'static>> {
    let Some(header) = card.header.as_ref() else {
        return Vec::new();
    };
    let label = match card.usage {
        Usage::Current => "Context",
        Usage::Pending => "Last reported context",
    };
    let mut spans = vec![
        label.bold(),
        "  ".into(),
        Span::from(format_with_separators(header.reported)),
    ];
    if let Some(window) = header.window {
        spans.push(" / ".dim());
        spans.push(Span::from(format_with_separators(window)).dim());
    }
    if let Some(percent) = header.percent_remaining {
        spans.push("   ".into());
        spans.push(Span::from(format!("{percent}% remaining")).dim());
    }
    vec![spans.into()]
}

fn startup_block(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    match &card.startup {
        Startup::Unavailable => {
            vec!["Startup context unavailable for this session".dim().into()]
        }
        Startup::Available {
            total,
            instructions,
            baseline,
            first_request_input,
        } => {
            let labels = [
                "Startup context",
                "  instructions",
                "  system + tools baseline (reconciled)",
            ];
            let numbers = [
                estimated(*total),
                estimated(*instructions),
                estimated(*baseline),
            ];
            let number_width = widest(numbers.iter().map(String::as_str));
            let label_width =
                widest(labels.iter().copied()).min(label_budget(inner_width, GAP + number_width));
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (position, (label, number)) in labels.iter().zip(numbers.iter()).enumerate() {
                let label = shorten(label, label_width);
                let label_span = if position == 0 {
                    label.clone().bold()
                } else {
                    Span::from(label.clone())
                };
                lines.push(
                    vec![
                        label_span,
                        Span::from(pad_to(&label, label_width)),
                        "  ".into(),
                        Span::from(pad_left(number, number_width)),
                    ]
                    .into(),
                );
            }
            let measured = format_with_separators(*first_request_input);
            lines.push(
                format!("  \u{21b3} measured: first request = {measured} input tokens")
                    .dim()
                    .into(),
            );
            lines
        }
    }
}

/// Widths shared by the attributed table, its total, the not-attributable rows, and the footer.
struct TableWidths {
    name: usize,
    number: usize,
    /// The gap plus percent column, or zero when no row carries a share.
    share: usize,
}

impl TableWidths {
    fn row_width(&self) -> usize {
        2 + self.name + GAP + self.number + self.share
    }
}

fn table_widths(card: &ContextCard, inner_width: usize) -> TableWidths {
    let footer_name = card.footer.map(|_| "Reported context");
    let names = card
        .categories
        .iter()
        .chain(card.not_attributable.iter())
        .map(|row| row.name)
        .chain(["total"])
        .chain(footer_name);
    let mut numbers: Vec<String> = card
        .categories
        .iter()
        .map(|row| cost_text(row.tokens))
        .collect();
    numbers.push(cost_text(card.attributed_total));
    numbers.extend(
        card.not_attributable
            .iter()
            .map(|row| cost_text(row.tokens)),
    );
    if let Some(reported) = card.footer {
        numbers.push(format_with_separators(reported));
    }
    let has_shares = card.footer.is_some()
        || card
            .categories
            .iter()
            .chain(card.not_attributable.iter())
            .any(|row| row.share_percent.is_some());
    let share = if has_shares { GAP + SHARE_WIDTH } else { 0 };
    let number = widest(numbers.iter().map(String::as_str));
    let name = widest(names).min(label_budget(inner_width, 2 + GAP + number + share));
    TableWidths {
        name,
        number,
        share,
    }
}

fn attributed_block(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    if card.categories.is_empty() {
        return Vec::new();
    }
    let widths = table_widths(card, inner_width);
    let largest_share = card
        .categories
        .iter()
        .filter_map(|row| row.share_percent)
        .max()
        .unwrap_or(0);

    let mut heading = vec!["Attributed to items".bold()];
    if widths.share > 0 {
        let gap = widths
            .row_width()
            .saturating_sub(SHARE_HEADING.len())
            .saturating_sub("Attributed to items".len())
            .max(1);
        heading.push(Span::from(" ".repeat(gap)));
        heading.push(SHARE_HEADING.dim());
    }
    let bar_cells = inner_width.saturating_sub(widths.row_width() + GAP);
    let mut lines: Vec<Line<'static>> = vec![heading.into()];
    for row in &card.categories {
        lines.push(table_row(
            row,
            &widths,
            bar(row.share_percent, largest_share, bar_cells),
        ));
    }
    lines.push(rule(&widths));
    lines.push(number_row(
        "total",
        cost_text(card.attributed_total),
        /*share*/ None,
        &widths,
    ));
    lines
}

/// Rendered whenever usage is stale, whether or not any category row exists.
fn pending_block(card: &ContextCard) -> Vec<Line<'static>> {
    if card.usage != Usage::Pending {
        return Vec::new();
    }
    vec![
        "Awaiting updated usage; run /ctx again after the response completes"
            .dim()
            .into(),
    ]
}

fn not_attributable_block(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    if card.not_attributable.is_empty() && card.footer.is_none() {
        return Vec::new();
    }
    let widths = table_widths(card, inner_width);
    let mut lines: Vec<Line<'static>> = card
        .not_attributable
        .iter()
        .map(|row| table_row(row, &widths, /*bar*/ None))
        .collect();
    if let Some(reported) = card.footer {
        lines.push(rule(&widths));
        lines.push(number_row(
            "Reported context",
            format_with_separators(reported),
            Some(100),
            &widths,
        ));
    }
    lines
}

fn contributors_block(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    if card.contributors.is_empty() {
        return Vec::new();
    }
    let category_width = widest(card.contributors.iter().map(|entry| entry.category));
    let numbers: Vec<String> = card
        .contributors
        .iter()
        .map(|entry| cost_text(entry.tokens))
        .collect();
    let number_width = widest(numbers.iter().map(String::as_str));
    let has_shares = card
        .contributors
        .iter()
        .any(|entry| entry.share_percent.is_some());
    let share_width = if has_shares { GAP + SHARE_WIDTH } else { 0 };
    let fixed = 2 + 1 + GAP + category_width + GAP + GAP + number_width + share_width;
    let label_width = widest(card.contributors.iter().map(|entry| entry.label.as_str()))
        .min(label_budget(inner_width, fixed));

    let mut heading = vec!["Largest contributors".bold()];
    if card
        .contributors
        .iter()
        .any(|entry| matches!(entry.tokens, TokenCost::Estimated(_)))
    {
        heading.push(" (estimated)".dim());
    }
    let mut lines: Vec<Line<'static>> = vec![heading.into()];
    for entry in &card.contributors {
        lines.push(contributor_row(
            entry,
            category_width,
            label_width,
            number_width,
        ));
    }
    lines
}

fn contributor_row(
    entry: &Contributor,
    category_width: usize,
    label_width: usize,
    number_width: usize,
) -> Line<'static> {
    let label = shorten(&entry.label, label_width);
    let mut spans = vec![
        Span::from(format!("  {}  ", entry.rank)),
        Span::from(entry.category.to_string()).dim(),
        Span::from(pad_to(entry.category, category_width)),
        "  ".into(),
        Span::from(label.clone()),
        Span::from(pad_to(&label, label_width)),
        "  ".into(),
        Span::from(pad_left(&cost_text(entry.tokens), number_width)),
    ];
    if let Some(percent) = entry.share_percent {
        spans.push("  ".into());
        spans.push(Span::from(pad_left(&format!("{percent}%"), SHARE_WIDTH)).dim());
    }
    spans.into()
}

fn turns_block(card: &ContextCard, inner_width: usize) -> Vec<Line<'static>> {
    if card.turns.is_empty() {
        return Vec::new();
    }
    let numbers: Vec<String> = card
        .turns
        .iter()
        .map(|turn| (turn.index + 1).to_string())
        .collect();
    let added: Vec<String> = card.turns.iter().map(added_text).collect();
    let context: Vec<String> = card
        .turns
        .iter()
        .map(|turn| {
            turn.context_after
                .map(format_with_separators)
                .unwrap_or_default()
        })
        .collect();
    let added_width = widest(added.iter().map(String::as_str)).max("added".len());
    let context_width = widest(context.iter().map(String::as_str)).max("context".len());

    let count = card.turns.len();
    let heading_text = if count == 1 {
        "Last turn".to_string()
    } else {
        format!("Last {count} turns")
    };
    // The turn column is as wide as the heading so the column heads sit over their values.
    let number_width = 2 + widest(numbers.iter().map(String::as_str));
    let turn_width = number_width.max(heading_text.len()).min(
        inner_width
            .saturating_sub(GAP + added_width + GAP + context_width)
            .max(number_width),
    );
    let heading = shorten(&heading_text, turn_width);
    let mut lines: Vec<Line<'static>> = vec![
        vec![
            heading.clone().bold(),
            Span::from(pad_to(&heading, turn_width)),
            "  ".into(),
            Span::from(pad_left("added", added_width)).dim(),
            "  ".into(),
            Span::from(pad_left("context", context_width)).dim(),
        ]
        .into(),
    ];
    for ((number, added), context) in numbers.iter().zip(added.iter()).zip(context.iter()) {
        lines.push(
            vec![
                Span::from(pad_right(&format!("  {number}"), turn_width)),
                "  ".into(),
                Span::from(pad_left(added, added_width)),
                "  ".into(),
                Span::from(pad_left(context, context_width)).dim(),
            ]
            .into(),
        );
    }
    lines
}

fn warnings_block(card: &ContextCard) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if card.warnings > 0 {
        lines.push(
            format!("{} items with classification warnings", card.warnings)
                .dim()
                .into(),
        );
    }
    if card.unsizable > 0 {
        lines.push(
            format!("{} items could not be sized", card.unsizable)
                .dim()
                .into(),
        );
    }
    lines
}

fn table_row(row: &Row, widths: &TableWidths, bar: Option<String>) -> Line<'static> {
    let mut line = number_row(row.name, cost_text(row.tokens), row.share_percent, widths);
    if let Some(bar) = bar {
        line.spans.push("  ".into());
        line.spans.push(Span::from(bar).dim());
    }
    line
}

fn number_row(
    name: &str,
    number: String,
    share: Option<u32>,
    widths: &TableWidths,
) -> Line<'static> {
    let name = shorten(name, widths.name);
    let mut spans = vec![
        Span::from(format!("  {}", pad_right(&name, widths.name))),
        "  ".into(),
        Span::from(pad_left(&number, widths.number)),
    ];
    if let Some(percent) = share {
        spans.push("  ".into());
        spans.push(Span::from(pad_left(&format!("{percent}%"), SHARE_WIDTH)).dim());
    }
    spans.into()
}

fn rule(widths: &TableWidths) -> Line<'static> {
    "\u{2500}".repeat(widths.row_width()).dim().into()
}

/// The bar scales to whatever room is left, so shares stay comparable at any width.
fn bar(share: Option<u32>, largest: u32, max_cells: usize) -> Option<String> {
    let share = share?;
    let full = BAR_CELLS.min(max_cells);
    if share == 0 || largest == 0 || full == 0 {
        return None;
    }
    let cells = (f64::from(share) / f64::from(largest) * full as f64).round() as usize;
    Some("\u{2588}".repeat(cells.max(1)))
}

fn added_text(turn: &TurnRow) -> String {
    match turn.added {
        None => NO_ANCHOR.to_string(),
        Some(added) if added >= 0 => format!("+{}", format_with_separators(added)),
        Some(added) => format_with_separators(added),
    }
}

fn cost_text(cost: TokenCost) -> String {
    match cost {
        TokenCost::Exact(tokens) => format_with_separators(tokens),
        TokenCost::Estimated(tokens) => estimated(tokens),
    }
}

fn estimated(tokens: i64) -> String {
    format!("~{}", format_with_separators(tokens))
}

fn widest<'a>(values: impl Iterator<Item = &'a str>) -> usize {
    values.map(display_width).max().unwrap_or(0)
}

/// What is left of `inner_width` for a label column once its row's other columns are taken.
fn label_budget(inner_width: usize, fixed: usize) -> usize {
    inner_width.saturating_sub(fixed).max(1)
}

fn shorten(text: &str, width: usize) -> String {
    truncate_line_with_ellipsis_if_overflow(Line::from(text.to_string()), width)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Trailing padding that brings `text` up to `width`.
fn pad_to(text: &str, width: usize) -> String {
    " ".repeat(width.saturating_sub(display_width(text)))
}

fn pad_right(text: &str, width: usize) -> String {
    format!("{text}{}", pad_to(text, width))
}

fn pad_left(text: &str, width: usize) -> String {
    format!("{}{text}", pad_to(text, width))
}

#[cfg(test)]
#[path = "card_tests.rs"]
mod tests;
