//! ratatui terminal front-end (Phase 10 stretch) over the same [`AppState`].
//!
//! Keys: `q` quit · `↑/↓` move · `←/→` switch class tab · `space` expand/collapse
//! a `ClassPtr` · `e` edit the selected value · `a` edit the address expression
//! · `m` toggle the memory map · `r` refresh regions.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TRow, Table};
use ratatui::{Frame, Terminal};

use reclass_backend_vmem::VmemBackend;
use reclass_core::{IntWidth, Node, NodeKind, Row};

use crate::app_state::AppState;

type Term = Terminal<CrosstermBackend<Stdout>>;

enum Input {
    None,
    Value,
    Expr,
}

struct Tui {
    state: AppState,
    selected: usize,
    input: Input,
    buffer: String,
    show_map: bool,
    quit: bool,
}

/// Run the terminal UI, optionally attaching to `pid` first.
pub fn run(pid: Option<i32>, addr: Option<String>) -> anyhow::Result<()> {
    let mut state = AppState::new();
    let c1 = state.add_class("Class1");
    for i in 0..16 {
        let _ = state.push_node(
            c1,
            Node::new(format!("field_{:X}", i * 8), NodeKind::Hex(IntWidth::W64)),
        );
    }
    if let Some(addr) = addr {
        let _ = state.set_address_expr(c1, addr);
    }
    if let Some(pid) = pid {
        match VmemBackend::by_pid(pid) {
            Ok(b) => {
                state.set_backend(Box::new(b));
                state.status = format!("attached pid {pid}");
            }
            Err(e) => state.status = format!("attach failed: {e}"),
        }
    }

    let mut term = setup()?;
    let mut app = Tui {
        state,
        selected: 0,
        input: Input::None,
        buffer: String::new(),
        show_map: false,
        quit: false,
    };
    let res = app.run_loop(&mut term);
    teardown(&mut term)?;
    res
}

fn setup() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn teardown(term: &mut Term) -> anyhow::Result<()> {
    disable_raw_mode()?;
    term.backend_mut().execute(LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

impl Tui {
    fn run_loop(&mut self, term: &mut Term) -> anyhow::Result<()> {
        let mut last = Instant::now();
        loop {
            let rows: Vec<Row> = self
                .state
                .compute_rows()
                .into_iter()
                .filter(|r| r.root == self.state.selected_view)
                .collect();
            if self.selected >= rows.len() {
                self.selected = rows.len().saturating_sub(1);
            }
            term.draw(|f| self.draw(f, &rows))?;

            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    self.handle_key(key.code, &rows);
                    if self.quit {
                        return Ok(());
                    }
                }
            // periodic region refresh (every 2s)
            if last.elapsed() > Duration::from_secs(2) {
                self.state.refresh_regions();
                last = Instant::now();
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode, rows: &[Row]) {
        match &self.input {
            Input::Value | Input::Expr => self.handle_input_key(code, rows),
            Input::None => self.handle_nav_key(code, rows),
        }
    }

    fn handle_nav_key(&mut self, code: KeyCode, rows: &[Row]) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < rows.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Tab => {
                let n = self.state.project.views.len();
                if n > 0 {
                    self.state.selected_view = (self.state.selected_view + 1) % n;
                    self.selected = 0;
                }
            }
            KeyCode::Left => {
                let n = self.state.project.views.len();
                if n > 0 {
                    self.state.selected_view = (self.state.selected_view + n - 1) % n;
                    self.selected = 0;
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(row) = rows.get(self.selected)
                    && row.expandable {
                        self.state.toggle_expand(row.root, row.path.clone());
                    }
            }
            KeyCode::Char('e') => {
                if let Some(row) = rows.get(self.selected)
                    && row.kind.is_editable() {
                        self.buffer = String::new();
                        self.input = Input::Value;
                    }
            }
            KeyCode::Char('a') => {
                if let Some(cid) = self.state.selected_class() {
                    self.buffer = self
                        .state
                        .project
                        .registry
                        .get(cid)
                        .map(|c| c.address_expr.clone())
                        .unwrap_or_default();
                    self.input = Input::Expr;
                }
            }
            KeyCode::Char('m') => self.show_map = !self.show_map,
            KeyCode::Char('r') => self.state.refresh_regions(),
            _ => {}
        }
    }

    fn handle_input_key(&mut self, code: KeyCode, rows: &[Row]) {
        match code {
            KeyCode::Esc => {
                self.input = Input::None;
                self.buffer.clear();
            }
            KeyCode::Char(c) => self.buffer.push(c),
            KeyCode::Backspace => {
                self.buffer.pop();
            }
            KeyCode::Enter => {
                let buf = std::mem::take(&mut self.buffer);
                match self.input {
                    Input::Value => {
                        if let Some(row) = rows.get(self.selected) {
                            let kind = row.kind.clone();
                            let addr = row.address;
                            if let Err(e) = self.state.write_value(addr, &kind, &buf) {
                                self.state.status = format!("edit error: {e}");
                            } else {
                                self.state.status = "wrote value".to_string();
                            }
                        }
                    }
                    Input::Expr => {
                        if let Some(cid) = self.state.selected_class() {
                            let _ = self.state.set_address_expr(cid, buf);
                        }
                    }
                    Input::None => {}
                }
                self.input = Input::None;
            }
            _ => {}
        }
    }

    fn draw(&self, f: &mut Frame<'_>, rows: &[Row]) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);
        if self.show_map {
            self.draw_map(f, chunks[1]);
        } else {
            self.draw_table(f, chunks[1], rows);
        }
        self.draw_status(f, chunks[2]);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let class = self
            .state
            .selected_class()
            .and_then(|c| self.state.project.registry.get(c));
        let (name, expr) = match class {
            Some(c) => (c.name.clone(), c.address_expr.clone()),
            None => ("<none>".to_string(), String::new()),
        };
        let base = self
            .state
            .view_status
            .get(self.state.selected_view)
            .map(|s| s.base)
            .unwrap_or(0);
        let line = Line::from(vec![
            Span::styled(
                format!(" {name} "),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("expr=[{expr}] base=0x{base:X}")),
        ]);
        let p = Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .title("reclass-rs (tui)"),
        );
        f.render_widget(p, area);
    }

    fn draw_table(&self, f: &mut Frame<'_>, area: Rect, rows: &[Row]) {
        let trows = rows.iter().enumerate().map(|(i, r)| {
            let indent = "  ".repeat(r.depth as usize);
            let tri = if r.expandable {
                if r.expanded { "▼ " } else { "▶ " }
            } else {
                ""
            };
            let style = if i == self.selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            TRow::new(vec![
                Cell::from(format!("0x{:04X}", r.offset)),
                Cell::from(format!("0x{:012X}", r.address)),
                Cell::from(format!("{indent}{tri}{}", r.type_label)),
                Cell::from(r.name.clone()),
                Cell::from(r.value.clone()),
            ])
            .style(style)
        });
        let table = Table::new(
            trows,
            [
                Constraint::Length(8),
                Constraint::Length(16),
                Constraint::Length(24),
                Constraint::Length(18),
                Constraint::Min(10),
            ],
        )
        .header(
            TRow::new(vec!["Offset", "Address", "Type", "Name", "Value"])
                .style(Style::default().add_modifier(Modifier::UNDERLINED)),
        )
        .block(Block::default().borders(Borders::ALL));
        f.render_widget(table, area);
    }

    fn draw_map(&self, f: &mut Frame<'_>, area: Rect) {
        let trows = self.state.regions.iter().map(|r| {
            TRow::new(vec![
                Cell::from(format!("0x{:012X}", r.start)),
                Cell::from(format!("0x{:012X}", r.end)),
                Cell::from(r.perms.to_string()),
                Cell::from(r.path.clone().unwrap_or_default()),
            ])
        });
        let table = Table::new(
            trows,
            [
                Constraint::Length(16),
                Constraint::Length(16),
                Constraint::Length(6),
                Constraint::Min(10),
            ],
        )
        .header(TRow::new(vec!["Start", "End", "Perms", "Path"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Memory map (m to close)"),
        );
        f.render_widget(table, area);
    }

    fn draw_status(&self, f: &mut Frame<'_>, area: Rect) {
        let text = match &self.input {
            Input::Value => format!("edit value > {}_", self.buffer),
            Input::Expr => format!("edit expr > {}_", self.buffer),
            Input::None => format!(
                "{}  |  q quit  ↑↓ move  ←→ tab  space expand  e edit  a addr  m map  r refresh",
                self.state.status
            ),
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::Gray)),
            area,
        );
    }
}
