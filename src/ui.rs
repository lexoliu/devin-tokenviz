use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal};

use crate::data::{self, Report};
use crate::fmt;
use crate::pricing::{PriceBook, Pricing};

const C_INPUT: Color = Color::Cyan;
const C_CACHED: Color = Color::DarkGray;
const C_OUTPUT: Color = Color::Yellow;
const C_FREE: Color = Color::Green;
const C_DIM: Color = Color::DarkGray;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Recent,
    Cost,
    Tokens,
    Name,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            Self::Recent => Self::Cost,
            Self::Cost => Self::Tokens,
            Self::Tokens => Self::Name,
            Self::Name => Self::Recent,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Cost => "cost",
            Self::Tokens => "tokens",
            Self::Name => "name",
        }
    }
}

struct App {
    report: Report,
    dir: PathBuf,
    days: Option<u32>,
    book: PriceBook,
    sort: SortKey,
    table_state: TableState,
}

impl App {
    fn sort_sessions(&mut self) {
        let s = &mut self.report.sessions;
        match self.sort {
            SortKey::Recent => s.sort_by_key(|a| std::cmp::Reverse(a.last_ts)),
            SortKey::Cost => s.sort_by(|a, b| {
                b.list_cost
                    .partial_cmp(&a.list_cost)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortKey::Tokens => s.sort_by_key(|x| std::cmp::Reverse(x.usage.total())),
            SortKey::Name => s.sort_by(|a, b| a.name.cmp(&b.name)),
        }
    }

    fn reload(&mut self) {
        if let Ok(r) = data::load(&self.dir, &self.book, self.days) {
            self.report = r;
            self.sort_sessions();
            self.table_state.select(Some(0));
        }
    }
}

struct TuiGuard;

impl TuiGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

pub fn run(report: Report, dir: PathBuf, days: Option<u32>, book: PriceBook) -> io::Result<()> {
    let _guard = TuiGuard::new()?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App {
        report,
        dir,
        days,
        book,
        sort: SortKey::Recent,
        table_state: TableState::default().with_selected(0),
    };
    app.sort_sessions();

    loop {
        term.draw(|f| draw(f, &mut app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let n = app.report.sessions.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Down | KeyCode::Char('j') => {
                let i = app.table_state.selected().unwrap_or(0);
                app.table_state
                    .select(Some((i + 1).min(n.saturating_sub(1))));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let i = app.table_state.selected().unwrap_or(0);
                app.table_state.select(Some(i.saturating_sub(1)));
            }
            KeyCode::Char('g') | KeyCode::Home => app.table_state.select(Some(0)),
            KeyCode::Char('G') | KeyCode::End => app.table_state.select(Some(n.saturating_sub(1))),
            KeyCode::Char('s') => {
                app.sort = app.sort.next();
                app.sort_sessions();
                app.table_state.select(Some(0));
            }
            KeyCode::Char('r') => app.reload(),
            _ => {}
        }
    }
    Ok(())
}

fn draw(f: &mut Frame, app: &mut App) {
    let r = &app.report;
    let n_models = r.models.len() as u16;
    let area = f.area();

    // Drop the bar chart on short terminals so the tables keep room.
    let show_bars = area.height > 4 + n_models + 2 + n_models + 4 + 7;
    let chunks = if show_bars {
        Layout::vertical([
            Constraint::Length(4),            // summary
            Constraint::Length(n_models + 2), // distribution bars
            Constraint::Length(n_models + 4), // pricing table
            Constraint::Min(4),               // sessions
            Constraint::Length(1),            // footer
        ])
        .split(area)
    } else {
        Layout::vertical([
            Constraint::Length(4),
            Constraint::Length(0),
            Constraint::Length(n_models + 4),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area)
    };

    draw_summary(f, r, chunks[0]);
    if show_bars {
        draw_bars(f, r, chunks[1]);
    }
    draw_pricing(f, r, chunks[2]);
    draw_sessions(f, app, chunks[3]);
    draw_footer(f, app, chunks[4]);
}

fn draw_summary(f: &mut Frame, r: &Report, area: ratatui::layout::Rect) {
    let range = match (r.earliest, r.latest) {
        (Some(a), Some(b)) => format!("{} → {}", fmt::date(a), fmt::date(b)),
        _ => "—".to_string(),
    };
    let title = format!(
        " {} sessions · {} steps · {}",
        r.sessions.len(),
        r.total_steps,
        range
    );
    let lines = vec![
        Line::from(vec![
            Span::styled(
                fmt::tokens(r.total.total()) + " tokens",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("in ", Style::default().fg(C_INPUT)),
            Span::styled(fmt::tokens(r.total.input), Style::default().fg(C_INPUT)),
            Span::raw(" · "),
            Span::styled("cached ", Style::default().fg(C_CACHED)),
            Span::styled(fmt::tokens(r.total.cached), Style::default().fg(C_CACHED)),
            Span::raw(" · "),
            Span::styled("out ", Style::default().fg(C_OUTPUT)),
            Span::styled(fmt::tokens(r.total.output), Style::default().fg(C_OUTPUT)),
        ]),
        Line::from(vec![
            Span::raw("list (equiv.) "),
            Span::styled(fmt::money(r.list_cost), Style::default()),
            Span::raw("   actual "),
            Span::styled(
                fmt::money(r.actual_cost),
                Style::default().fg(if r.actual_cost == 0.0 {
                    C_FREE
                } else {
                    Color::Red
                }),
            ),
            if r.has_unpriced {
                Span::styled("   · some models unpriced", Style::default().fg(C_DIM))
            } else {
                Span::raw("")
            },
        ]),
    ];
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" devin-tokenviz ·{}", title)),
        ),
        area,
    );
}

fn draw_bars(f: &mut Frame, r: &Report, area: ratatui::layout::Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" tokens by model  "),
            Span::styled("■", Style::default().fg(C_INPUT)),
            Span::raw(" input  "),
            Span::styled("■", Style::default().fg(C_CACHED)),
            Span::raw(" cached  "),
            Span::styled("■", Style::default().fg(C_OUTPUT)),
            Span::raw(" output "),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if r.models.is_empty() {
        f.render_widget(Paragraph::new("no usage data"), inner);
        return;
    }

    let label_w = r
        .models
        .iter()
        .map(|m| m.label.len())
        .max()
        .unwrap_or(8)
        .min(22);
    let max_total = r
        .models
        .iter()
        .map(|m| m.usage.total())
        .max()
        .unwrap_or(1)
        .max(1);
    let grand = r.total.total().max(1);
    // " label" (label_w+1) + bar + " 216.9M  46.5%" (16)
    let bar_w = (inner.width as usize).saturating_sub(label_w + 1 + 16);

    let mut lines = Vec::new();
    for m in &r.models {
        let t = m.usage.total();
        let mut spans = vec![Span::raw(format!("{:<w$} ", m.label, w = label_w))];
        let bw = if t == 0 || bar_w == 0 {
            0
        } else {
            ((t as f64 / max_total as f64) * bar_w as f64)
                .round()
                .max(1.0) as usize
        };
        // proportional segments: input, cached, output
        let mut acc = 0usize;
        for (val, color) in [
            (m.usage.input, C_INPUT),
            (m.usage.cached, C_CACHED),
            (m.usage.output, C_OUTPUT),
        ] {
            let w = if val == 0 || bw == 0 {
                0
            } else {
                ((val as f64 / t as f64) * bw as f64).round() as usize
            };
            let w = w.min(bw.saturating_sub(acc));
            acc += w;
            if w > 0 {
                spans.push(Span::styled("█".repeat(w), Style::default().fg(color)));
            }
        }
        if acc < bw {
            spans.push(Span::styled(
                "█".repeat(bw - acc),
                Style::default().fg(C_OUTPUT),
            ));
        }
        spans.push(Span::raw(format!(
            " {:>8} {:>5.1}%",
            fmt::tokens(t),
            t as f64 / grand as f64 * 100.0
        )));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_pricing(f: &mut Frame, r: &Report, area: ratatui::layout::Rect) {
    let header = Row::new(
        [
            "Model",
            "Sess",
            "Priced as",
            "in",
            "cached",
            "out",
            "List cost",
            "Actual",
        ]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD))),
    );

    let rows: Vec<Row> = r
        .models
        .iter()
        .map(|m| {
            let (rates, list, actual) = match &m.price {
                Some(p) => (
                    vec![
                        fmt::money(p.input),
                        fmt::money(p.cached),
                        fmt::money(p.output),
                    ],
                    m.list_cost().map(fmt::money).unwrap_or_else(|| "?".into()),
                    match m.pricing {
                        Pricing::Paid => m.list_cost().map(fmt::money).unwrap_or_default(),
                        Pricing::Free { .. } => "$0.00".to_string(),
                        Pricing::Unpriced => "?".to_string(),
                    },
                ),
                None => (
                    vec!["?".into(), "?".into(), "?".into()],
                    "?".into(),
                    "?".into(),
                ),
            };
            let priced_as = match &m.pricing {
                Pricing::Free { billed_as } => billed_as.clone(),
                Pricing::Paid => "list".to_string(),
                Pricing::Unpriced => "?".to_string(),
            };
            let (list_style, actual_style) = match m.pricing {
                Pricing::Free { .. } => (
                    Style::default()
                        .fg(C_DIM)
                        .add_modifier(Modifier::CROSSED_OUT),
                    Style::default().fg(C_FREE),
                ),
                Pricing::Paid => (Style::default(), Style::default()),
                Pricing::Unpriced => (Style::default().fg(C_DIM), Style::default().fg(C_DIM)),
            };
            Row::new(vec![
                Cell::from(m.label.clone()),
                Cell::from(m.sessions.to_string()),
                Cell::from(priced_as).style(Style::default().fg(C_DIM)),
                Cell::from(rates[0].clone()).style(Style::default().fg(C_DIM)),
                Cell::from(rates[1].clone()).style(Style::default().fg(C_DIM)),
                Cell::from(rates[2].clone()).style(Style::default().fg(C_DIM)),
                Cell::from(list).style(list_style),
                Cell::from(actual).style(actual_style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(4),
            Constraint::Min(14),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(11),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" pricing — $ per 1M tokens "),
    );
    f.render_widget(table, area);
}

fn draw_sessions(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let header = Row::new(
        [
            "Session",
            "Last active",
            "Models",
            "Total",
            "List",
            "Actual",
        ]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD))),
    );

    let rows: Vec<Row> = app
        .report
        .sessions
        .iter()
        .map(|s| {
            let models = s.models.keys().cloned().collect::<Vec<_>>().join(", ");
            let last = s.last_ts.map(fmt::datetime).unwrap_or_else(|| "—".into());
            let (list_cell, actual_cell) = if s.list_cost > 0.0 && s.actual_cost == 0.0 {
                (
                    Cell::from(fmt::money(s.list_cost)).style(
                        Style::default()
                            .fg(C_DIM)
                            .add_modifier(Modifier::CROSSED_OUT),
                    ),
                    Cell::from("$0.00").style(Style::default().fg(C_FREE)),
                )
            } else if s.actual_cost > 0.0 {
                (
                    Cell::from(fmt::money(s.list_cost)),
                    Cell::from(fmt::money(s.actual_cost)),
                )
            } else {
                (
                    Cell::from(if s.has_unpriced { "?" } else { "$0.00" })
                        .style(Style::default().fg(C_DIM)),
                    Cell::from("$0.00").style(Style::default().fg(C_DIM)),
                )
            };
            Row::new(vec![
                Cell::from(s.name.clone()),
                Cell::from(last),
                Cell::from(models).style(Style::default().fg(C_DIM)),
                Cell::from(fmt::tokens(s.usage.total())),
                list_cell,
                actual_cell,
            ])
        })
        .collect();

    let title = format!(" sessions — sort: {} (s to change) ", app.sort.name());
    let table = Table::new(
        rows,
        [
            Constraint::Min(18),
            Constraint::Length(13),
            Constraint::Length(24),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(Block::default().borders(Borders::ALL).title(title));
    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_footer(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = vec![Span::styled(
        " q quit · ↑↓/jk select · g/G top/bottom · s sort · r reload ",
        Style::default().fg(C_DIM),
    )];
    if app.report.files_failed > 0 {
        spans.push(Span::styled(
            format!(
                "⚠ {} transcript(s) failed to parse",
                app.report.files_failed
            ),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
