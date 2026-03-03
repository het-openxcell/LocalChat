use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

use std::{io, net::TcpStream, sync::mpsc, time::Duration};

use chrono::Local;

use crate::crypto::CryptoSession;
use crate::history::HistoryWriter;

// ─── Types ───────────────────────────────────────────────────────────────────

pub struct ChatMessage {
    pub timestamp: String,
    pub sender: String,
    pub content: String,
    pub is_own: bool,
    pub is_system: bool,
}

pub enum AppEvent {
    Message(ChatMessage),
    PeerLeft,
    ConnectionLost,
}

#[derive(PartialEq, Clone, Copy)]
enum Status {
    Connected,
    PeerLeft,
    ConnectionLost,
    Quit,
}

// ─── App state ───────────────────────────────────────────────────────────────

struct App {
    messages: Vec<ChatMessage>,
    input: String,
    cursor_pos: usize,
    /// Number of messages from the bottom to skip when rendering (0 = latest).
    scroll_offset: usize,
    my_name: String,
    peer_name: String,
    status: Status,
}

impl App {
    fn new(my_name: String, peer_name: String) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            my_name,
            peer_name,
            status: Status::Connected,
        }
    }

    fn push(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
        // Keep auto-scroll unless user has scrolled up
        if self.scroll_offset == 0 {
            // Nothing to adjust — render will show latest
        }
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = (self.scroll_offset + n).min(self.messages.len());
    }

    fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub fn run_ui(
    stream: TcpStream,
    crypto: CryptoSession,
    event_rx: mpsc::Receiver<AppEvent>,
    my_name: String,
    peer_name: String,
    mut history: Option<HistoryWriter>,
) -> io::Result<()> {
    // Restore terminal on panic
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        default_panic(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(my_name, peer_name.clone());
    app.push(system_msg(format!(
        "🔒  End-to-end encrypted session with {}  started.",
        peer_name
    )));
    app.push(system_msg(
        "PageUp/PageDown to scroll  ·  /quit or Esc to exit".to_string(),
    ));

    let result = event_loop(&mut terminal, &mut app, stream, &crypto, &event_rx, &mut history);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Remove custom panic hook
    let _ = std::panic::take_hook();

    if let Some(h) = history.as_mut() {
        h.write_event("session ended");
    }

    result
}

// ─── Event loop ──────────────────────────────────────────────────────────────

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    mut stream: TcpStream,
    crypto: &CryptoSession,
    event_rx: &mpsc::Receiver<AppEvent>,
    history: &mut Option<HistoryWriter>,
) -> io::Result<()> {
    loop {
        // Drain network events
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                AppEvent::Message(msg) => {
                    send_desktop_notification(&msg.sender, &msg.content);
                    if let Some(h) = history.as_mut() {
                        h.write_message(&msg.timestamp, &msg.sender, &msg.content);
                    }
                    app.push(msg);
                }
                AppEvent::PeerLeft => {
                    app.push(system_msg(format!(
                        "{} has left the chat.",
                        app.peer_name
                    )));
                    app.status = Status::PeerLeft;
                }
                AppEvent::ConnectionLost => {
                    app.push(system_msg("Connection lost.".to_string()));
                    app.status = Status::ConnectionLost;
                }
            }
        }

        terminal.draw(|f| render(f, app))?;

        // After disconnect: wait for any key then exit
        if app.status != Status::Connected {
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(_) = event::read()? {
                    break;
                }
            }
            continue;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            match key.code {
                // ── Send ──────────────────────────────────────────────
                KeyCode::Enter => {
                    let text = app.input.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    if text == "/quit" {
                        crypto.send_msg(&mut stream, b"/quit").ok();
                        app.status = Status::Quit;
                        break;
                    }
                    match crypto.send_msg(&mut stream, text.as_bytes()) {
                        Ok(()) => {
                            let msg = own_msg(app.my_name.clone(), text.clone());
                            if let Some(h) = history.as_mut() {
                                h.write_message(&msg.timestamp, &msg.sender, &msg.content);
                            }
                            app.push(msg);
                        }
                        Err(_) => {
                            app.push(system_msg("Failed to send — connection may be broken.".to_string()));
                            app.status = Status::ConnectionLost;
                        }
                    }
                    app.input.clear();
                    app.cursor_pos = 0;
                    app.scroll_offset = 0; // jump to latest after sending
                }

                // ── Exit ──────────────────────────────────────────────
                KeyCode::Esc => {
                    crypto.send_msg(&mut stream, b"/quit").ok();
                    app.status = Status::Quit;
                    break;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    crypto.send_msg(&mut stream, b"/quit").ok();
                    app.status = Status::Quit;
                    break;
                }

                // ── Typing ────────────────────────────────────────────
                KeyCode::Char(c) => {
                    app.input.insert(app.cursor_pos, c);
                    app.cursor_pos += 1;
                }
                KeyCode::Backspace => {
                    if app.cursor_pos > 0 {
                        app.cursor_pos -= 1;
                        app.input.remove(app.cursor_pos);
                    }
                }
                KeyCode::Delete => {
                    if app.cursor_pos < app.input.len() {
                        app.input.remove(app.cursor_pos);
                    }
                }
                KeyCode::Left => {
                    if app.cursor_pos > 0 {
                        app.cursor_pos -= 1;
                    }
                }
                KeyCode::Right => {
                    if app.cursor_pos < app.input.len() {
                        app.cursor_pos += 1;
                    }
                }
                KeyCode::Home => {
                    app.cursor_pos = 0;
                }
                KeyCode::End => {
                    app.cursor_pos = app.input.len();
                }

                // ── Scroll ────────────────────────────────────────────
                KeyCode::PageUp => app.scroll_up(5),
                KeyCode::PageDown => app.scroll_down(5),
                KeyCode::Up => app.scroll_up(1),
                KeyCode::Down => app.scroll_down(1),

                _ => {}
            }
        }
    }

    Ok(())
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render(f: &mut Frame<'_>, app: &App) {
    let area = f.size();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // messages
            Constraint::Length(3), // input
        ])
        .split(area);

    render_header(f, app, chunks[0]);
    render_messages(f, app, chunks[1]);
    render_input(f, app, chunks[2]);
}

fn render_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let (status_str, status_color) = match app.status {
        Status::Connected => ("🔒 Encrypted · Connected", Color::Green),
        Status::PeerLeft => ("⚠  Peer disconnected", Color::Yellow),
        Status::ConnectionLost => ("✖  Connection lost", Color::Red),
        Status::Quit => ("Disconnecting…", Color::DarkGray),
    };

    let line = Line::from(vec![
        Span::styled(
            format!("  LocalChat  ·  {} ↔ {}  ", app.my_name, app.peer_name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("·  {}", status_str),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let header = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );

    f.render_widget(header, area);
}

fn render_messages(f: &mut Frame<'_>, app: &App, area: Rect) {
    // How many message rows fit inside the block (excluding top/bottom borders)
    let inner_height = area.height.saturating_sub(2) as usize;

    let total = app.messages.len();
    let end_idx = total.saturating_sub(app.scroll_offset);
    let start_idx = end_idx.saturating_sub(inner_height);
    let visible = &app.messages[start_idx..end_idx];

    let lines: Vec<Line<'_>> = visible.iter().map(format_msg).collect();

    let scrolled = app.scroll_offset > 0;
    let title = if scrolled {
        format!(
            " Messages  ↑ scrolled ({} newer) ↓ ",
            app.scroll_offset
        )
    } else {
        " Messages ".to_string()
    };

    let para = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title(title.as_str())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(para, area);
}

fn render_input(f: &mut Frame<'_>, app: &App, area: Rect) {
    let hint = match app.status {
        Status::Connected => " /quit · Esc · Ctrl+C to exit ",
        _ => " Press any key to close ",
    };

    let prefix = "> ";
    let display = format!("{}{}", prefix, app.input);

    let input_style = if app.status == Status::Connected {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let widget = Paragraph::new(display.as_str()).block(
        Block::default()
            .title(hint)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if app.status == Status::Connected {
                Color::Cyan
            } else {
                Color::DarkGray
            })),
    ).style(input_style);

    f.render_widget(widget, area);

    // Place the cursor inside the input box
    if app.status == Status::Connected {
        f.set_cursor(
            area.x + 1 + prefix.len() as u16 + app.cursor_pos as u16,
            area.y + 1,
        );
    }
}

// ─── Message formatting ───────────────────────────────────────────────────────

fn format_msg(msg: &ChatMessage) -> Line<'_> {
    if msg.is_system {
        return Line::from(vec![Span::styled(
            format!("  ─  {}  ─", msg.content),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )]);
    }

    let (name_color, content_color) = if msg.is_own {
        (Color::Green, Color::White)
    } else {
        (Color::Cyan, Color::Gray)
    };

    Line::from(vec![
        Span::styled(
            format!(" [{}] ", msg.timestamp),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<14}", msg.sender),
            Style::default()
                .fg(name_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(msg.content.as_str(), Style::default().fg(content_color)),
    ])
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn system_msg(content: String) -> ChatMessage {
    ChatMessage {
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        sender: String::new(),
        content,
        is_own: false,
        is_system: true,
    }
}

fn own_msg(sender: String, content: String) -> ChatMessage {
    ChatMessage {
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        sender,
        content,
        is_own: true,
        is_system: false,
    }
}

/// Fires a desktop notification via `notify-send` (available on all Linux desktops).
/// Silently ignores failures (e.g. when running headless).
fn send_desktop_notification(sender: &str, content: &str) {
    let _ = std::process::Command::new("notify-send")
        .args([
            "--icon=user-available",
            "--urgency=normal",
            "--expire-time=5000",
            &format!("LocalChat — {}", sender),
            content,
        ])
        .spawn();
}
