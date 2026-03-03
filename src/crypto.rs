use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::{rngs::OsRng, RngCore};
use x25519_dalek::{EphemeralSecret, PublicKey};

use std::io::{self, BufRead, Write};

/// Holds the symmetric key derived from the DH exchange.
/// Two separate instances (one per direction) share the same key — safe because
/// each encrypt call generates a fresh random nonce.
pub struct CryptoSession {
    key: [u8; 32],
}

impl CryptoSession {
    pub fn new(shared_secret: [u8; 32]) -> Self {
        Self { key: shared_secret }
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(&self.key))
    }

    /// Returns [12-byte nonce || ciphertext].
    pub fn encrypt(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher()
            .encrypt(nonce, plaintext)
            .expect("encryption failed");
        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        out
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, &'static str> {
        if data.len() < 12 {
            return Err("payload too short");
        }
        let nonce = Nonce::from_slice(&data[..12]);
        self.cipher()
            .decrypt(nonce, &data[12..])
            .map_err(|_| "decryption failed — message may be tampered")
    }

    /// Encrypts `plaintext`, base64-encodes it, and writes a single line.
    pub fn send_msg<W: Write>(&self, w: &mut W, plaintext: &[u8]) -> io::Result<()> {
        let encrypted = self.encrypt(plaintext);
        writeln!(w, "{}", B64.encode(&encrypted))
    }

    /// Reads one base64 line and decrypts it.
    pub fn recv_msg<R: BufRead>(&self, r: &mut R) -> io::Result<Vec<u8>> {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        let encrypted = B64
            .decode(line.trim())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.decrypt(&encrypted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// X25519 Diffie-Hellman handshake over a line-oriented stream.
/// Both sides simultaneously write their public key (base64) then read the peer's.
/// Returns the 32-byte shared secret.
pub fn perform_key_exchange<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> io::Result<[u8; 32]> {
    let my_secret = EphemeralSecret::random_from_rng(OsRng);
    let my_public = PublicKey::from(&my_secret);

    // Send our public key first (TCP is full-duplex; no deadlock risk for 44 bytes)
    writeln!(writer, "{}", B64.encode(my_public.as_bytes()))?;

    // Read peer's public key
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer disconnected during key exchange",
        ));
    }

    let peer_bytes = B64
        .decode(line.trim())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let peer_bytes: [u8; 32] = peer_bytes.try_into().map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "invalid public key length")
    })?;

    let peer_public = PublicKey::from(peer_bytes);
    let shared = my_secret.diffie_hellman(&peer_public);
    Ok(*shared.as_bytes())
}
