mod crypto;
mod history;
mod server;
mod ui;

use std::io::{self, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use chrono::Local;

use crypto::{perform_key_exchange, CryptoSession};
use history::{history_dir_display, HistoryWriter};
use ui::{AppEvent, ChatMessage};

const PORT: u16 = 7777;

// ─── ANSI (setup phase; ratatui takes over after connecting) ──────────────────
const R: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const BG: &str = "\x1b[48;5;235m";

// ─── Terminal helpers ────────────────────────────────────────────────────────

fn clear() {
    print!("\x1b[2J\x1b[H");
    io::stdout().flush().unwrap();
}

fn banner() {
    let pad = " ".repeat(62);
    println!("{BG}{pad}{R}");
    println!("{BG}{BOLD}{CYAN}  ██╗      ██████╗  ██████╗ █████╗ ██╗      ██████╗██╗  ██╗ {R}");
    println!("{BG}{BOLD}{CYAN}  ██║     ██╔═══██╗██╔════╝██╔══██╗██║     ██╔════╝██║  ██║ {R}");
    println!("{BG}{BOLD}{CYAN}  ██║     ██║   ██║██║     ███████║██║     ██║     ███████║ {R}");
    println!("{BG}{BOLD}{CYAN}  ██║     ██║   ██║██║     ██╔══██║██║     ██║     ██╔══██║ {R}");
    println!("{BG}{BOLD}{CYAN}  ███████╗╚██████╔╝╚██████╗██║  ██║███████╗╚██████╗██║  ██║ {R}");
    println!("{BG}{BOLD}{CYAN}  ╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝╚══════╝ ╚═════╝╚═╝  ╚═╝ {R}");
    println!("{BG}{DIM}{YELLOW}       encrypted  ·  local  ·  multi-user  ·  v3.0           {R}");
    println!("{BG}{pad}{R}");
    println!();
}

fn prompt(label: &str) -> String {
    print!("  {BOLD}{MAGENTA}{label}{R} › ");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    line.trim().to_string()
}

fn info(msg: &str) { println!("  {BOLD}{CYAN}ℹ{R}  {msg}"); }
fn ok(msg: &str)   { println!("  {BOLD}{GREEN}✔{R}  {msg}"); }
fn warn(msg: &str) { println!("  {BOLD}{YELLOW}⚠{R}  {msg}"); }
fn err(msg: &str)  { println!("  {BOLD}{RED}✖{R}  {msg}"); }

fn divider(label: &str) {
    println!("\n  {DIM}{CYAN}── {label} ──{R}");
}

fn get_local_ip() -> String {
    use std::net::UdpSocket;
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:80")?; s.local_addr() })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

// ─── HOST mode (multi-user server + host participates in chat) ────────────────

fn host_mode(my_name: String) {
    let local_ip = get_local_ip();
    divider("HOST MODE  —  multi-user server");
    info(&format!("Your LAN IP  →  {BOLD}{YELLOW}{local_ip}{R}"));
    info(&format!("Listening on port  {BOLD}{YELLOW}{PORT}{R}"));
    warn("Share your IP — multiple people can join at once.");
    println!();

    let bind_addr = format!("0.0.0.0:{PORT}");
    let listener = match TcpListener::bind(&bind_addr) {
        Ok(l) => l,
        Err(e) => { err(&format!("Cannot bind to {bind_addr}: {e}")); return; }
    };

    ok("Server is ready!  Waiting for connections…");
    info(&format!("History saved to  {DIM}{}{R}", history_dir_display()));
    println!();

    // Channels: UI → server (send), server → UI (events)
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>();
    let (send_tx, send_rx)   = mpsc::channel::<String>();

    // Run the server + broadcast loop in a background thread
    let server_name = my_name.clone();
    let event_tx_server = event_tx;
    thread::spawn(move || {
        server::run_server(server_name, listener, event_tx_server, send_rx);
    });

    let history = HistoryWriter::new(&my_name, "ChatRoom");

    if let Err(e) = ui::run_ui(
        send_tx,
        event_rx,
        my_name.clone(),
        "Chat Room".to_string(),
        history,
    ) {
        err(&format!("UI error: {e}"));
    }
}

// ─── JOIN mode (client connecting to a multi-user server) ────────────────────

fn client_mode(my_name: String) {
    divider("JOIN MODE");

    let ip = prompt("Host's local IP address");
    if ip.is_empty() { err("No IP entered."); return; }

    let addr = format!("{}:{PORT}", ip.trim());
    info(&format!("Connecting to {addr}…"));

    let stream = match TcpStream::connect(&addr) {
        Ok(s) => { ok(&format!("TCP connected to {BOLD}{addr}{R}")); s }
        Err(e) => {
            err(&format!("Connection failed: {e}"));
            warn("Make sure the host has started in HOST mode first.");
            return;
        }
    };

    run_session(stream, my_name);
}

/// Sets up crypto + name exchange, then hands off to the ratatui UI.
fn run_session(stream: TcpStream, my_name: String) {
    stream.set_nodelay(true).ok();

    let read_half = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => { err(&format!("Stream clone failed: {e}")); return; }
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;

    // ── Key exchange ──────────────────────────────────────────────────────────
    info("X25519 key exchange…");
    let secret = match perform_key_exchange(&mut reader, &mut writer) {
        Ok(s) => s,
        Err(e) => { err(&format!("Key exchange failed: {e}")); return; }
    };

    let crypto_hs   = CryptoSession::new(secret); // handshake writes
    let crypto_rx   = CryptoSession::new(secret); // receiver thread
    let crypto_send = CryptoSession::new(secret); // sender thread

    // ── Name handshake ────────────────────────────────────────────────────────
    if crypto_hs.send_msg(&mut writer, format!("HELLO:{my_name}").as_bytes()).is_err() {
        err("Name exchange failed."); return;
    }

    let server_name = match crypto_rx.recv_msg(&mut reader) {
        Ok(d) => String::from_utf8_lossy(&d)
            .trim_start_matches("HELLO:")
            .trim()
            .to_string(),
        Err(e) => { err(&format!("Name exchange failed: {e}")); return; }
    };

    // ── ROOM message (initial online members, sent right after handshake) ─────
    let mut initial_users: Vec<String> = match crypto_rx.recv_msg(&mut reader) {
        Ok(d) => {
            let text = String::from_utf8_lossy(&d).to_string();
            text.strip_prefix("ROOM|")
                .map(|rest| rest.split('|').map(str::to_string).collect())
                .unwrap_or_default()
        }
        Err(_) => vec![],
    };
    if !initial_users.contains(&my_name) {
        initial_users.push(my_name.clone());
    }
    initial_users.sort_unstable();

    ok(&format!(
        "{BOLD}{GREEN}Encrypted channel established!{R}  Room hosted by {BOLD}{CYAN}{server_name}{R}"
    ));
    info(&format!("History saved to  {DIM}{}{R}", history_dir_display()));
    println!();

    // ── Channels ──────────────────────────────────────────────────────────────
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>();
    let (send_tx, send_rx)   = mpsc::channel::<String>();

    // Send initial online list to UI
    event_tx.send(AppEvent::RoomInfo(initial_users)).ok();

    // ── Sender thread: UI → server ────────────────────────────────────────────
    thread::spawn(move || {
        for text in send_rx {
            let payload = if text == "/quit" { b"/quit".as_slice() } else { text.as_bytes() };
            if crypto_send.send_msg(&mut writer, payload).is_err() {
                break;
            }
            if text == "/quit" { break; }
        }
    });

    // ── Receiver thread: server → UI ─────────────────────────────────────────
    let event_tx_rx = event_tx;
    thread::spawn(move || loop {
        match crypto_rx.recv_msg(&mut reader) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data).to_string();
                if text == "/quit" {
                    event_tx_rx.send(AppEvent::PeerLeft).ok();
                    break;
                }
                parse_server_msg(&text, &event_tx_rx);
            }
            Err(_) => {
                event_tx_rx.send(AppEvent::ConnectionLost).ok();
                break;
            }
        }
    });

    // ── Launch TUI ────────────────────────────────────────────────────────────
    let history = HistoryWriter::new(&my_name, &server_name);

    if let Err(e) = ui::run_ui(send_tx, event_rx, my_name, server_name, history) {
        err(&format!("UI error: {e}"));
    }
}

/// Decode a server protocol message and dispatch the matching AppEvent.
fn parse_server_msg(text: &str, tx: &mpsc::Sender<AppEvent>) {
    let parts: Vec<&str> = text.splitn(3, '|').collect();
    match parts.as_slice() {
        ["MSG", sender, content] => {
            tx.send(AppEvent::Message(ChatMessage {
                timestamp: Local::now().format("%H:%M:%S").to_string(),
                sender: sender.to_string(),
                content: content.to_string(),
                is_own: false,
                is_system: false,
            }))
            .ok();
        }
        ["JOIN", name] => {
            tx.send(AppEvent::UserJoined(name.to_string())).ok();
        }
        ["LEAVE", name] => {
            tx.send(AppEvent::UserLeft(name.to_string())).ok();
        }
        ["ROOM", ..] => {
            // Re-sync: parse the member list and send RoomInfo
            let names: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
            tx.send(AppEvent::RoomInfo(names)).ok();
        }
        _ => {} // unknown / ignore
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        eprintln!("\nCrash: {info}");
    }));

    clear();
    banner();

    let my_name = loop {
        let n = prompt("Your name");
        if !n.is_empty() { break n; }
        warn("Name cannot be empty.");
    };

    println!();
    divider("choose a role");
    println!("  {BOLD}1){R} HOST  — start a multi-user server, share your IP");
    println!("  {BOLD}2){R} JOIN  — enter the host's IP and connect");
    println!();

    loop {
        match prompt("Enter 1 or 2").to_lowercase() {
            s if s == "1" || s == "host" => { host_mode(my_name); break; }
            s if s == "2" || s == "join" => { client_mode(my_name); break; }
            _ => warn("Please enter 1 (host) or 2 (join)."),
        }
    }

    println!();
    ok("Goodbye!");
    println!();
}
