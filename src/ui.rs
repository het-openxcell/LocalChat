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

use std::{io, sync::mpsc, time::Duration};

use chrono::Local;

use crate::history::HistoryWriter;

// ─── Public types ─────────────────────────────────────────────────────────────

pub struct ChatMessage {
    pub timestamp: String,
    pub sender: String,
    pub content: String,
    pub is_own: bool,
    pub is_system: bool,
}

pub enum AppEvent {
    /// A chat message arrived from a peer.
    Message(ChatMessage),
    /// The other side sent /quit (graceful disconnect).
    PeerLeft,
    /// Network error.
    ConnectionLost,
    /// A user joined the room (multi-user).
    UserJoined(String),
    /// A user left the room (multi-user).
    UserLeft(String),
    /// Initial room membership (sent once on connect).
    RoomInfo(Vec<String>),
}

// ─── App state ────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Status {
    Connected,
    PeerLeft,
    ConnectionLost,
    Quit,
}

struct App {
    messages: Vec<ChatMessage>,
    input: String,
    cursor_pos: usize,
    /// How many messages from the bottom to hide (0 = always show latest).
    scroll_offset: usize,
    my_name: String,
    /// "Alice" in 1-on-1, "Chat Room" when hosting.
    room_label: String,
    /// Sorted list of users currently online (including self).
    online_users: Vec<String>,
    status: Status,
}

impl App {
    fn new(my_name: String, room_label: String) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            online_users: vec![my_name.clone()],
            my_name,
            room_label,
            status: Status::Connected,
        }
    }

    fn push(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = (self.scroll_offset + n).min(self.messages.len());
    }

    fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    fn set_online(&mut self, mut users: Vec<String>) {
        if !users.contains(&self.my_name) {
            users.push(self.my_name.clone());
        }
        users.sort_unstable();
        self.online_users = users;
    }

    fn user_join(&mut self, name: String) {
        if !self.online_users.contains(&name) {
            self.online_users.push(name);
            self.online_users.sort_unstable();
        }
    }

    fn user_leave(&mut self, name: &str) {
        self.online_users.retain(|n| n != name);
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Launch the chat UI.
///
/// * `send_tx`    – send a message text (UI → transport); send "/quit" to disconnect.
/// * `event_rx`   – receive events from the transport layer (messages, join/leave, …).
/// * `room_label` – displayed in the header. Peer's name in 1-on-1, "Chat Room" in multi-user.
pub fn run_ui(
    send_tx: mpsc::Sender<String>,
    event_rx: mpsc::Receiver<AppEvent>,
    my_name: String,
    room_label: String,
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

    let mut app = App::new(my_name.clone(), room_label.clone());
    app.push(system_msg(format!(
        "🔒  End-to-end encrypted.  Connected to: {}",
        room_label
    )));
    app.push(system_msg(
        "PageUp/PageDown to scroll  ·  /quit or Esc to exit".to_string(),
    ));

    let result = event_loop(
        &mut terminal,
        &mut app,
        &send_tx,
        &event_rx,
        &mut history,
    );

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    let _ = std::panic::take_hook();

    if let Some(h) = history.as_mut() {
        h.write_event("session ended");
    }

    result
}

// ─── Event loop ───────────────────────────────────────────────────────────────

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    send_tx: &mpsc::Sender<String>,
    event_rx: &mpsc::Receiver<AppEvent>,
    history: &mut Option<HistoryWriter>,
) -> io::Result<()> {
    loop {
        // ── Drain incoming events ─────────────────────────────────────────────
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
                    app.push(system_msg("The other side disconnected.".to_string()));
                    app.status = Status::PeerLeft;
                }
                AppEvent::ConnectionLost => {
                    app.push(system_msg("Connection lost.".to_string()));
                    app.status = Status::ConnectionLost;
                }
                AppEvent::UserJoined(name) => {
                    send_desktop_notification("LocalChat", &format!("{} joined", name));
                    app.push(system_msg(format!("{} joined the room.", name)));
                    app.user_join(name);
                }
                AppEvent::UserLeft(name) => {
                    app.push(system_msg(format!("{} left the room.", name)));
                    app.user_leave(&name);
                }
                AppEvent::RoomInfo(names) => {
                    app.set_online(names);
                }
            }
        }

        terminal.draw(|f| render(f, app))?;

        // ── Disconnected: wait for any key then exit ───────────────────────────
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

        // ── Keyboard input ────────────────────────────────────────────────────
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => {
                    let text = app.input.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }

                    if text == "/quit" {
                        send_tx.send("/quit".to_string()).ok();
                        app.status = Status::Quit;
                        break;
                    }

                    if send_tx.send(text.clone()).is_ok() {
                        let msg = own_msg(app.my_name.clone(), text.clone());
                        if let Some(h) = history.as_mut() {
                            h.write_message(&msg.timestamp, &msg.sender, &msg.content);
                        }
                        app.push(msg);
                    } else {
                        app.push(system_msg(
                            "Send failed — connection may be broken.".to_string(),
                        ));
                        app.status = Status::ConnectionLost;
                    }

                    app.input.clear();
                    app.cursor_pos = 0;
                    app.scroll_offset = 0;
                }

                KeyCode::Esc => {
                    send_tx.send("/quit".to_string()).ok();
                    app.status = Status::Quit;
                    break;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    send_tx.send("/quit".to_string()).ok();
                    app.status = Status::Quit;
                    break;
                }

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
                KeyCode::Home => app.cursor_pos = 0,
                KeyCode::End => app.cursor_pos = app.input.len(),

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

// ─── Rendering ────────────────────────────────────────────────────────────────

fn render(f: &mut Frame<'_>, app: &App) {
    let area = f.size();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // body
            Constraint::Length(3), // input
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(1),     // messages
            Constraint::Length(20), // online users
        ])
        .split(vertical[1]);

    render_header(f, app, vertical[0]);
    render_messages(f, app, body[0]);
    render_users(f, app, body[1]);
    render_input(f, app, vertical[2]);
}

fn render_header(f: &mut Frame<'_>, app: &App, area: Rect) {
    let (status_str, status_color) = match app.status {
        Status::Connected => ("🔒 Encrypted · Connected", Color::Green),
        Status::PeerLeft => ("⚠  Peer disconnected", Color::Yellow),
        Status::ConnectionLost => ("✖  Connection lost", Color::Red),
        Status::Quit => ("Disconnecting…", Color::DarkGray),
    };

    let n = app.online_users.len();
    let user_count = if n == 1 {
        "1 user".to_string()
    } else {
        format!("{n} users")
    };

    let line = Line::from(vec![
        Span::styled(
            format!("  LocalChat  ·  {}  ·  {}  ", app.my_name, app.room_label),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("·  {}  ·  {}", status_str, user_count),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    f.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        ),
        area,
    );
}

fn render_messages(f: &mut Frame<'_>, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;

    let total = app.messages.len();
    let end_idx = total.saturating_sub(app.scroll_offset);
    let start_idx = end_idx.saturating_sub(inner_height);
    let visible = &app.messages[start_idx..end_idx];

    let lines: Vec<Line<'_>> = visible.iter().map(format_msg).collect();

    let title = if app.scroll_offset > 0 {
        format!(" Messages  ↑ ({} newer) ", app.scroll_offset)
    } else {
        " Messages ".to_string()
    };

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title.as_str())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_users(f: &mut Frame<'_>, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;

    let lines: Vec<Line<'_>> = app
        .online_users
        .iter()
        .take(inner_height)
        .map(|name| {
            let is_self = name == &app.my_name;
            let (icon, color) = if is_self {
                ("▶ ", Color::Green)
            } else {
                ("● ", Color::Cyan)
            };
            Line::from(vec![
                Span::styled(icon, Style::default().fg(color)),
                Span::styled(
                    name.as_str(),
                    Style::default()
                        .fg(color)
                        .add_modifier(if is_self { Modifier::BOLD } else { Modifier::empty() }),
                ),
            ])
        })
        .collect();

    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(format!(" Online ({}) ", app.online_users.len()).as_str())
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        ),
        area,
    );
}

fn render_input(f: &mut Frame<'_>, app: &App, area: Rect) {
    let hint = match app.status {
        Status::Connected => " /quit · Esc · Ctrl+C to exit ",
        _ => " Press any key to close ",
    };

    let prefix = "> ";
    let display = format!("{}{}", prefix, app.input);

    let border_color = if app.status == Status::Connected {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let text_color = if app.status == Status::Connected {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    f.render_widget(
        Paragraph::new(display.as_str())
            .block(
                Block::default()
                    .title(hint)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color)),
            )
            .style(Style::default().fg(text_color)),
        area,
    );

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

pub fn system_msg(content: String) -> ChatMessage {
    ChatMessage {
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        sender: String::new(),
        content,
        is_own: false,
        is_system: true,
    }
}

pub fn own_msg(sender: String, content: String) -> ChatMessage {
    ChatMessage {
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        sender,
        content,
        is_own: true,
        is_system: false,
    }
}

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
