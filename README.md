# 💬 LocalChat — P2P Terminal Messenger

A zero-dependency, two-person terminal chat app for local networks.  
Pure Rust standard library only — no external crates needed.

---

## Quick Start

### Prerequisites
- [Rust](https://rustup.rs/) installed (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)

### Build & Run

```bash
cargo run --release
```

---

## How It Works

```
Person A (HOST)                    Person B (JOIN)
─────────────────                  ─────────────────
cargo run                          cargo run
Enter name: Alice                  Enter name: Bob
Choose role: 1 (HOST)              Choose role: 2 (JOIN)
→ Shows your LAN IP                Enter Alice's IP: 192.168.1.42
→ Waits for connection...          → Connects instantly
          ↕  TCP socket on port 7777  ↕
             Full-duplex messaging
```

### Step by step

1. **Person A** runs `cargo run`, picks `1 (HOST)`. The app prints their local IP.
2. Person A shares that IP with Person B (verbally, phone, whatever).
3. **Person B** runs `cargo run`, picks `2 (JOIN)`, enters Person A's IP.
4. Chat opens — both can type freely and simultaneously.
5. Type `/quit` to disconnect gracefully.

---

## Features

- 🎨 Coloured terminal UI (ANSI — works on macOS, Linux, Windows Terminal)
- 🕒 Timestamps on every message
- ↕ Full-duplex: both people can type at the same time
- 🔌 Graceful `/quit` command
- 📡 Auto-detects your LAN IP
- 🚫 Zero external dependencies

---

## Network Notes

- Uses **TCP port 7777**. Make sure it's not blocked by a firewall.
- Both machines must be on the **same local network** (same Wi-Fi / LAN).
- If you're on separate networks, you'd need port forwarding (out of scope here).

---

## Troubleshooting

| Problem | Fix |
|---|---|
| "Connection refused" | Make sure HOST has started and is waiting before JOIN connects |
| "Cannot bind" | Port 7777 is in use — kill the other process or change `PORT` in `main.rs` |
| Can't find your IP | Run `ip route get 1` (Linux) or `ipconfig` (Windows) manually |
