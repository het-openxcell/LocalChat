mod crypto;
mod history;
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

// ─── ANSI (setup phase only — ratatui takes over after connection) ────────────
const R: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const BG: &str = "\x1b[48;5;235m";

// ─── Setup helpers ────────────────────────────────────────────────────────────

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
    println!("{BG}{DIM}{YELLOW}       encrypted  ·  local  ·  p2p  messenger  v2.0          {R}");
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

fn info(msg: &str) {
    println!("  {BOLD}{CYAN}ℹ{R}  {msg}");
}

fn ok(msg: &str) {
    println!("  {BOLD}{GREEN}✔{R}  {msg}");
}

fn warn(msg: &str) {
    println!("  {BOLD}{YELLOW}⚠{R}  {msg}");
}

fn err(msg: &str) {
    println!("  {BOLD}{RED}✖{R}  {msg}");
}

fn divider(label: &str) {
    println!("\n  {DIM}{CYAN}── {label} ──{R}");
}

fn get_local_ip() -> String {
    use std::net::UdpSocket;
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

// ─── Session setup (key exchange, name exchange, spawn threads) ───────────────

fn run_session(stream: TcpStream, my_name: String) {
    stream.set_nodelay(true).ok();

    // Split into separate reader/writer handles
    let read_half = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            err(&format!("Stream clone failed: {e}"));
            return;
        }
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;

    // ── Key exchange ─────────────────────────────────────────────────────────
    info("Performing X25519 key exchange…");
    let shared_secret = match perform_key_exchange(&mut reader, &mut writer) {
        Ok(s) => s,
        Err(e) => {
            err(&format!("Key exchange failed: {e}"));
            return;
        }
    };

    let crypto_send = CryptoSession::new(shared_secret);
    let crypto_recv = CryptoSession::new(shared_secret);

    // ── Name exchange (encrypted) ─────────────────────────────────────────────
    if let Err(e) = crypto_send.send_msg(&mut writer, format!("HELLO:{my_name}").as_bytes()) {
        err(&format!("Name exchange send failed: {e}"));
        return;
    }

    let peer_name = match crypto_recv.recv_msg(&mut reader) {
        Ok(data) => {
            let s = String::from_utf8_lossy(&data).to_string();
            s.strip_prefix("HELLO:")
                .unwrap_or("Unknown")
                .trim()
                .to_string()
        }
        Err(e) => {
            err(&format!("Name exchange recv failed: {e}"));
            return;
        }
    };

    ok(&format!(
        "{BOLD}{GREEN}Encrypted channel established!{R}  Chatting with {BOLD}{CYAN}{peer_name}{R}"
    ));
    info(&format!(
        "History saved to  {DIM}{}{R}",
        history_dir_display()
    ));
    println!();

    // ── Receiver thread ───────────────────────────────────────────────────────
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let peer_clone = peer_name.clone();

    thread::spawn(move || loop {
        match crypto_recv.recv_msg(&mut reader) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data).to_string();
                if text == "/quit" {
                    tx.send(AppEvent::PeerLeft).ok();
                    break;
                }
                let msg = ChatMessage {
                    timestamp: Local::now().format("%H:%M:%S").to_string(),
                    sender: peer_clone.clone(),
                    content: text,
                    is_own: false,
                    is_system: false,
                };
                tx.send(AppEvent::Message(msg)).ok();
            }
            Err(_) => {
                tx.send(AppEvent::ConnectionLost).ok();
                break;
            }
        }
    });

    // ── Launch TUI ────────────────────────────────────────────────────────────
    let history = HistoryWriter::new(&my_name, &peer_name);

    if let Err(e) = ui::run_ui(writer, crypto_send, rx, my_name, peer_name, history) {
        // Terminal already restored at this point
        err(&format!("UI error: {e}"));
    }
}

// ─── Host / Client modes ──────────────────────────────────────────────────────

fn host_mode(my_name: String) {
    let local_ip = get_local_ip();
    divider("HOST MODE");
    info(&format!(
        "Your LAN IP  →  {BOLD}{YELLOW}{local_ip}{R}"
    ));
    info(&format!("Listening on port {BOLD}{YELLOW}{PORT}{R}"));
    warn(&format!(
        "Share your IP with the other person and wait for them to connect."
    ));
    println!();

    let bind_addr = format!("0.0.0.0:{PORT}");
    let listener = match TcpListener::bind(&bind_addr) {
        Ok(l) => l,
        Err(e) => {
            err(&format!("Cannot bind to {bind_addr}: {e}"));
            return;
        }
    };

    println!("  {DIM}{YELLOW}⏳  Waiting for connection…{R}");

    match listener.accept() {
        Ok((stream, addr)) => {
            ok(&format!("Peer connected from {BOLD}{}{R}!", addr.ip()));
            run_session(stream, my_name);
        }
        Err(e) => {
            err(&format!("Accept failed: {e}"));
        }
    }
}

fn client_mode(my_name: String) {
    divider("JOIN MODE");

    let ip = prompt("Peer's local IP address");
    if ip.is_empty() {
        err("No IP entered.");
        return;
    }

    let addr = format!("{}:{PORT}", ip.trim());
    info(&format!("Connecting to {addr}…"));

    match TcpStream::connect(&addr) {
        Ok(stream) => {
            ok(&format!("TCP connection established to {BOLD}{addr}{R}"));
            run_session(stream, my_name);
        }
        Err(e) => {
            err(&format!("Connection failed: {e}"));
            warn("Make sure the other person has started in HOST mode first.");
        }
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    // Restore terminal on panic (belt-and-suspenders alongside the hook in ui.rs)
    std::panic::set_hook(Box::new(|info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::terminal::LeaveAlternateScreen
        );
        eprintln!("\nCrash: {info}");
    }));

    clear();
    banner();

    let my_name = loop {
        let n = prompt("Your name");
        if !n.is_empty() {
            break n;
        }
        warn("Name cannot be empty.");
    };

    println!();
    divider("choose a role");
    println!("  {BOLD}1){R} HOST  — start the server and share your IP");
    println!("  {BOLD}2){R} JOIN  — enter the other person's IP and connect");
    println!();

    loop {
        match prompt("Enter 1 or 2").to_lowercase() {
            s if s == "1" || s == "host" => {
                host_mode(my_name);
                break;
            }
            s if s == "2" || s == "join" => {
                client_mode(my_name);
                break;
            }
            _ => warn("Please enter 1 (host) or 2 (join)."),
        }
    }

    println!();
    ok("Goodbye!");
    println!();
}
