/// Multi-user server: accepts unlimited clients, routes messages between everyone
/// including the host who participates as a first-class member.
///
/// Wire protocol (plaintext before encryption):
///   Client → Server  : raw message text  |  "/quit"
///   Server → Client  : "MSG|<sender>|<content>"
///                    | "JOIN|<name>"
///                    | "LEAVE|<name>"
///                    | "ROOM|<n1>|<n2>|…"   (sent once on connect)
use std::collections::HashMap;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::sync::mpsc;
use std::thread;

use chrono::Local;

use crate::crypto::{perform_key_exchange, CryptoSession};
use crate::ui::{AppEvent, ChatMessage};

// ─── Shared server state ─────────────────────────────────────────────────────

struct ClientHandle {
    name: String,
    /// Plaintext messages queued for this client (writer thread encrypts & sends).
    tx: mpsc::Sender<String>,
}

pub struct ServerState {
    clients: Mutex<HashMap<u64, ClientHandle>>,
    next_id: AtomicU64,
    /// Channel to the host's own UI.
    event_tx: mpsc::Sender<AppEvent>,
    pub host_name: String,
}

impl ServerState {
    pub fn new(host_name: String, event_tx: mpsc::Sender<AppEvent>) -> Arc<Self> {
        Arc::new(Self {
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            event_tx,
            host_name,
        })
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send plaintext to every client except `exclude_id`.
    /// Pass `u64::MAX` to broadcast to absolutely everyone.
    pub fn broadcast(&self, exclude_id: u64, msg: &str) {
        let clients = self.clients.lock().unwrap();
        for (id, client) in clients.iter() {
            if *id != exclude_id {
                client.tx.send(msg.to_string()).ok();
            }
        }
    }

}

// ─── Per-client handler ───────────────────────────────────────────────────────

pub fn handle_client(stream: TcpStream, server: Arc<ServerState>) {
    stream.set_nodelay(true).ok();

    let client_id = server.alloc_id();

    let read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(read_stream);
    let mut writer = stream;

    // ── Key exchange ──────────────────────────────────────────────────────────
    let secret = match perform_key_exchange(&mut reader, &mut writer) {
        Ok(s) => s,
        Err(_) => return,
    };

    let crypto_hs = CryptoSession::new(secret); // for initial sends before writer thread
    let crypto_rx = CryptoSession::new(secret); // for reader loop
    let crypto_tx = CryptoSession::new(secret); // for writer thread

    // ── Name handshake ────────────────────────────────────────────────────────
    if crypto_hs
        .send_msg(&mut writer, format!("HELLO:{}", server.host_name).as_bytes())
        .is_err()
    {
        return;
    }

    let client_name = match crypto_rx.recv_msg(&mut reader) {
        Ok(d) => String::from_utf8_lossy(&d)
            .trim_start_matches("HELLO:")
            .trim()
            .to_string(),
        Err(_) => return,
    };

    if client_name.is_empty() {
        return;
    }

    // ── Per-client send channel ───────────────────────────────────────────────
    let (tx, rx) = mpsc::channel::<String>();

    // ── Register client, send room info ──────────────────────────────────────
    {
        let mut clients = server.clients.lock().unwrap();

        // List of everyone currently online (all existing clients + host)
        let mut online: Vec<String> = clients.values().map(|c| c.name.clone()).collect();
        online.push(server.host_name.clone());
        online.sort_unstable();

        let room_msg = format!("ROOM|{}", online.join("|"));
        crypto_hs.send_msg(&mut writer, room_msg.as_bytes()).ok();

        clients.insert(client_id, ClientHandle { name: client_name.clone(), tx });
    }

    // Announce to existing clients and host UI
    server.broadcast(client_id, &format!("JOIN|{}", client_name));
    server.event_tx.send(AppEvent::UserJoined(client_name.clone())).ok();

    // ── Writer thread (takes ownership of `writer`) ───────────────────────────
    thread::spawn(move || {
        for plaintext in rx {
            if crypto_tx
                .send_msg(&mut writer, plaintext.as_bytes())
                .is_err()
            {
                break;
            }
        }
        // `writer` dropped here → TCP write side closed
    });

    // ── Reader loop (runs in the current thread) ──────────────────────────────
    loop {
        match crypto_rx.recv_msg(&mut reader) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data).to_string();
                if text == "/quit" {
                    break;
                }

                let ts = Local::now().format("%H:%M:%S").to_string();

                // Relay to all other clients
                server.broadcast(client_id, &format!("MSG|{}|{}", client_name, text));

                // Show in host's UI
                server
                    .event_tx
                    .send(AppEvent::Message(ChatMessage {
                        timestamp: ts,
                        sender: client_name.clone(),
                        content: text,
                        is_own: false,
                        is_system: false,
                    }))
                    .ok();
            }
            Err(_) => break,
        }
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    {
        let mut clients = server.clients.lock().unwrap();
        clients.remove(&client_id);
        // Dropping ClientHandle drops `tx` → channel closes → writer thread exits
    }

    server.broadcast(u64::MAX, &format!("LEAVE|{}", client_name));
    server.event_tx.send(AppEvent::UserLeft(client_name)).ok();
}

// ─── Accept + broadcast loop ──────────────────────────────────────────────────

/// Runs the server: spawns an accept loop, then enters the host-broadcast loop.
/// `send_rx` receives plaintext from the host's own UI.
/// Blocks until `send_rx` is closed (host quit).
pub fn run_server(
    host_name: String,
    listener: TcpListener,
    event_tx: mpsc::Sender<AppEvent>,
    send_rx: mpsc::Receiver<String>,
) {
    let server = ServerState::new(host_name.clone(), event_tx);

    // Accept loop in background
    let server_accept = Arc::clone(&server);
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let srv = Arc::clone(&server_accept);
                    thread::spawn(move || handle_client(s, srv));
                }
                Err(_) => break,
            }
        }
    });

    // Host broadcast loop (blocks until UI quits)
    for text in send_rx {
        if text == "/quit" {
            server.broadcast(u64::MAX, &format!("LEAVE|{}", host_name));
            break;
        }
        server.broadcast(u64::MAX, &format!("MSG|{}|{}", host_name, text));
    }
    // Remaining client connections will detect closed channels and clean up
}
