use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::{
    codec::{decode_frame, encode_frame, Compression, PacketFrame},
    io::{PacketReader, PacketWriter, ProtocolError, Result},
    v1_8_9::{
        packets::{ClientboundLoginPacket, ClientboundPlayPacket, NextState, ServerboundPacket},
        PROTOCOL_VERSION,
    },
};

/// Result of a 1.8 server-list ping: the raw status JSON (version, players,
/// MOTD "description") plus the measured round-trip latency.
#[derive(Debug, Clone)]
pub struct StatusPing {
    pub json: String,
    pub latency_ms: u32,
}

/// Ping a server with the 1.8 status handshake: Handshake(next_state=Status),
/// Status Request (0x00) → Status Response JSON, then Ping (0x01) → Pong for
/// the latency. The whole exchange is bounded by `timeout`.
pub fn ping_status_1_8_9(host: &str, port: u16, timeout: Duration) -> Result<StatusPing> {
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or(ProtocolError::InvalidData("address resolved to nothing"))?;
    let stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut client = BlockingClient::from_stream(stream)?;

    client.write_packet(
        ServerboundPacket::Handshake {
            protocol_version: PROTOCOL_VERSION,
            host: host.to_owned(),
            port,
            next_state: NextState::Status,
        }
        .into_frame(),
    )?;
    client.write_packet(PacketFrame::new(0x00, Vec::new()))?; // status request
    let request_sent = Instant::now();
    let frame = client.read_frame()?;
    if frame.id != 0x00 {
        return Err(ProtocolError::InvalidPacketId(frame.id, "status/clientbound"));
    }
    let json = PacketReader::new(&frame.body).read_string(32767)?;

    // Ping/pong round trip for the latency; fall back to the status round
    // trip when the server skips the pong.
    let mut ping = PacketWriter::new();
    ping.write_i64(0);
    let latency_ms = match client
        .write_packet(PacketFrame::new(0x01, ping.into_inner()))
        .and_then(|()| {
            let sent = Instant::now();
            client.read_frame().map(|frame| (frame, sent))
        }) {
        Ok((frame, sent)) if frame.id == 0x01 => sent.elapsed().as_millis() as u32,
        _ => request_sent.elapsed().as_millis() as u32,
    };

    Ok(StatusPing { json, latency_ms })
}

#[derive(Debug)]
pub struct OfflineLoginResult {
    pub uuid: String,
    pub username: String,
    pub compression: Compression,
}

/// A fully authenticated Minecraft session obtained via Microsoft OAuth.
/// Pass to `login_premium_1_8_9` to join online-mode servers.
#[derive(Debug, Clone)]
pub struct PremiumSession {
    /// Minecraft `access_token` from the Minecraft Services API.
    pub access_token: String,
    /// UUID **without** dashes (as returned by the profile endpoint).
    pub uuid: String,
    /// In-game username.
    pub username: String,
}

/// Vanilla's frame limit: `NettyCompressionDecoder` rejects an uncompressed
/// length above this, and the frame-length VarInt is at most 3 bytes (< 2^21).
pub const MAX_PACKET_LEN: usize = 2_097_152;

/// The inbound half of a connection: frame reassembly, decryption, inflate.
pub struct ClientReader {
    stream: TcpStream,
    compression: Compression,
    read_buf: Vec<u8>,
    /// AES-128-CFB8 decryptor enabled after `EncryptionResponse`.
    decryptor: Option<crate::crypto::Aes128Cfb8>,
}

/// The outbound half of a connection: deflate, encryption, socket write.
pub struct ClientWriter {
    stream: TcpStream,
    compression: Compression,
    /// AES-128-CFB8 encryptor enabled after `EncryptionResponse`.
    encryptor: Option<crate::crypto::Aes128Cfb8>,
}

pub struct BlockingClient {
    reader: ClientReader,
    writer: ClientWriter,
}

impl BlockingClient {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Self::from_stream(stream)
    }

    fn from_stream(stream: TcpStream) -> Result<Self> {
        Ok(Self {
            writer: ClientWriter {
                stream: stream.try_clone()?,
                compression: Compression::Disabled,
                encryptor: None,
            },
            reader: ClientReader {
                stream,
                compression: Compression::Disabled,
                read_buf: Vec::new(),
                decryptor: None,
            },
        })
    }

    /// Split into independently owned read and write halves (same socket), so
    /// one thread can block on reads while another writes immediately. Call
    /// after login: compression and encryption state is fixed from here on.
    pub fn into_split(self) -> (ClientReader, ClientWriter) {
        (self.reader, self.writer)
    }

    fn set_compression(&mut self, compression: Compression) {
        self.reader.compression = compression;
        self.writer.compression = compression;
    }

    /// Offline (cracked) login — no encryption; works with `online-mode=false` servers.
    pub fn login_offline_1_8_9(
        &mut self,
        host: &str,
        port: u16,
        username: &str,
    ) -> Result<OfflineLoginResult> {
        self.write_packet(
            ServerboundPacket::Handshake {
                protocol_version: PROTOCOL_VERSION,
                host: host.to_owned(),
                port,
                next_state: NextState::Login,
            }
            .into_frame(),
        )?;
        self.write_packet(
            ServerboundPacket::LoginStart {
                username: username.to_owned(),
            }
            .into_frame(),
        )?;

        loop {
            let frame = self.read_frame()?;
            match ClientboundLoginPacket::from_frame(frame)? {
                ClientboundLoginPacket::SetCompression { threshold } => {
                    self.set_compression(Compression::Enabled { threshold });
                }
                ClientboundLoginPacket::LoginSuccess { uuid, username } => {
                    return Ok(OfflineLoginResult {
                        uuid,
                        username,
                        compression: self.reader.compression,
                    });
                }
                ClientboundLoginPacket::Disconnect { reason_json } => {
                    return Err(ProtocolError::Compression(reason_json));
                }
                ClientboundLoginPacket::EncryptionRequest { .. } => {
                    return Err(ProtocolError::InvalidData(
                        "online-mode encryption is not implemented yet",
                    ));
                }
            }
        }
    }

    /// Premium (online-mode) login implementing the full 1.8 encryption handshake:
    ///
    /// 1. Send Handshake (next_state=Login) + LoginStart.
    /// 2. Read `EncryptionRequest` { server_id, public_key (X.509 DER), verify_token }.
    /// 3. Generate 16-byte random shared secret.
    /// 4. Compute the Mojang server hash (SHA-1, signed-big-endian hex).
    /// 5. POST `https://sessionserver.mojang.com/session/minecraft/join`.
    /// 6. RSA-PKCS#1v1.5 encrypt shared secret + verify token; send `EncryptionResponse`.
    /// 7. Enable AES-128-CFB8 on all subsequent reads and writes.
    /// 8. Read `SetCompression` (optional) then `LoginSuccess`.
    pub fn login_premium_1_8_9(
        &mut self,
        host: &str,
        port: u16,
        session: &PremiumSession,
    ) -> Result<OfflineLoginResult> {
        self.write_packet(
            ServerboundPacket::Handshake {
                protocol_version: PROTOCOL_VERSION,
                host: host.to_owned(),
                port,
                next_state: NextState::Login,
            }
            .into_frame(),
        )?;
        self.write_packet(
            ServerboundPacket::LoginStart {
                username: session.username.clone(),
            }
            .into_frame(),
        )?;

        // First packet from an online-mode server is EncryptionRequest.
        let frame = self.read_frame()?;
        let (server_id, public_key_der, verify_token) =
            match ClientboundLoginPacket::from_frame(frame)? {
                ClientboundLoginPacket::EncryptionRequest {
                    server_id,
                    public_key,
                    verify_token,
                } => (server_id, public_key, verify_token),
                ClientboundLoginPacket::Disconnect { reason_json } => {
                    return Err(ProtocolError::Compression(reason_json));
                }
                // Offline server that doesn't encrypt — accept it.
                ClientboundLoginPacket::LoginSuccess { uuid, username } => {
                    return Ok(OfflineLoginResult {
                        uuid,
                        username,
                        compression: self.reader.compression,
                    });
                }
                ClientboundLoginPacket::SetCompression { .. } => {
                    return Err(ProtocolError::InvalidData(
                        "unexpected SetCompression before EncryptionRequest",
                    ));
                }
            };

        // ── Generate 16-byte random shared secret ────────────────────────────
        let shared_secret: [u8; 16] = {
            use rand::RngCore;
            let mut buf = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut buf);
            buf
        };

        // ── Compute Minecraft server hash ─────────────────────────────────────
        let server_hash = minecraft_server_hash(&server_id, &shared_secret, &public_key_der)
            .map_err(|_| ProtocolError::InvalidData("failed to compute server hash"))?;

        // ── Authenticate with Mojang session server ───────────────────────────
        let join_payload = serde_json::json!({
            "accessToken": session.access_token,
            "selectedProfile": session.uuid,
            "serverId": server_hash,
        });
        ureq::post("https://sessionserver.mojang.com/session/minecraft/join")
            .set("Content-Type", "application/json")
            .send_json(join_payload)
            .map_err(|e| ProtocolError::Compression(format!("session join failed: {e}")))?;

        // ── RSA-PKCS#1v1.5 encrypt shared secret and verify token ─────────────
        use rsa::pkcs8::DecodePublicKey;
        let rsa_pub = rsa::RsaPublicKey::from_public_key_der(&public_key_der)
            .map_err(|_| ProtocolError::InvalidData("failed to parse server public key"))?;
        let mut rng = rand::thread_rng();
        let enc_secret = rsa_pub
            .encrypt(&mut rng, rsa::pkcs1v15::Pkcs1v15Encrypt, &shared_secret)
            .map_err(|_| ProtocolError::InvalidData("RSA encrypt secret failed"))?;
        let enc_token = rsa_pub
            .encrypt(&mut rng, rsa::pkcs1v15::Pkcs1v15Encrypt, &verify_token)
            .map_err(|_| ProtocolError::InvalidData("RSA encrypt token failed"))?;

        // ── Send EncryptionResponse (login packet id 0x01) ────────────────────
        self.write_packet(encryption_response_frame(enc_secret, enc_token))?;

        // ── Enable AES-128-CFB8 on both stream directions ────────────────────
        self.writer.encryptor = Some(crate::crypto::Aes128Cfb8::encryptor(&shared_secret));
        self.reader.decryptor = Some(crate::crypto::Aes128Cfb8::decryptor(&shared_secret));

        // ── Read post-encryption login packets ────────────────────────────────
        loop {
            let frame = self.read_frame()?;
            match ClientboundLoginPacket::from_frame(frame)? {
                ClientboundLoginPacket::SetCompression { threshold } => {
                    self.set_compression(Compression::Enabled { threshold });
                }
                ClientboundLoginPacket::LoginSuccess { uuid, username } => {
                    return Ok(OfflineLoginResult {
                        uuid,
                        username,
                        compression: self.reader.compression,
                    });
                }
                ClientboundLoginPacket::Disconnect { reason_json } => {
                    return Err(ProtocolError::Compression(reason_json));
                }
                ClientboundLoginPacket::EncryptionRequest { .. } => {
                    return Err(ProtocolError::InvalidData(
                        "unexpected second EncryptionRequest",
                    ));
                }
            }
        }
    }

    pub fn read_play_packet_1_8_9(&mut self) -> Result<ClientboundPlayPacket> {
        self.reader.read_play_packet_1_8_9()
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.reader.stream.set_read_timeout(timeout)?;
        Ok(())
    }

    pub fn write_packet(&mut self, frame: PacketFrame) -> Result<()> {
        self.writer.write_packet(frame)
    }

    fn read_frame(&mut self) -> Result<PacketFrame> {
        self.reader.read_frame()
    }
}

impl ClientWriter {
    pub fn write_packet(&mut self, frame: PacketFrame) -> Result<()> {
        let mut encoded = encode_frame(&frame, self.compression)?;
        if let Some(enc) = &mut self.encryptor {
            enc.process(&mut encoded);
        }
        self.stream.write_all(&encoded)?;
        Ok(())
    }

    /// Shut down both directions of the socket, unblocking a reader parked in
    /// a blocking read on the other half.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl ClientReader {
    pub fn read_play_packet_1_8_9(&mut self) -> Result<ClientboundPlayPacket> {
        ClientboundPlayPacket::from_frame(self.read_frame()?)
    }

    fn read_frame(&mut self) -> Result<PacketFrame> {
        loop {
            if let Some((length, header_len)) = read_var_i32_prefix(&self.read_buf)? {
                if length < 0 {
                    return Err(ProtocolError::InvalidData("negative packet length"));
                }
                // An oversized length would otherwise grow `read_buf` without
                // bound waiting for bytes that may never come.
                if length as usize > MAX_PACKET_LEN {
                    return Err(ProtocolError::InvalidData("packet length exceeds 2097152"));
                }

                let frame_len = header_len + length as usize;
                if self.read_buf.len() >= frame_len {
                    let data = self.read_buf[header_len..frame_len].to_vec();
                    self.read_buf.drain(..frame_len);
                    return decode_frame(&data, self.compression);
                }
            }

            self.read_more()?;
        }
    }

    fn read_more(&mut self) -> Result<()> {
        let mut buf = [0u8; 4096];
        let n = self.stream.read(&mut buf)?;
        if n == 0 {
            return Err(ProtocolError::Io("connection closed".to_owned()));
        }
        let mut data = buf[..n].to_vec();
        if let Some(dec) = &mut self.decryptor {
            dec.process(&mut data);
        }
        self.read_buf.extend_from_slice(&data);
        Ok(())
    }
}

fn read_var_i32_prefix(data: &[u8]) -> Result<Option<(i32, usize)>> {
    let mut result = 0i32;
    let mut shift = 0u32;
    for (index, b) in data.iter().copied().enumerate().take(5) {
        result |= ((b & 0x7f) as i32) << shift;
        if b & 0x80 == 0 {
            return Ok(Some((result, index + 1)));
        }
        shift += 7;
    }
    if data.len() >= 5 {
        Err(ProtocolError::VarIntTooLong)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod frame_read_tests {
    use super::*;

    #[test]
    fn read_var_i32_prefix_waits_for_complete_varint() {
        assert_eq!(read_var_i32_prefix(&[0x80]).unwrap(), None);
        assert_eq!(read_var_i32_prefix(&[0x80, 0x01]).unwrap(), Some((128, 2)));
    }

    /// A frame length above vanilla's limit is rejected up front instead of
    /// buffering forever (or until allocation fails) for its body.
    #[test]
    fn oversized_frame_length_is_rejected() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = BlockingClient::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let mut len = PacketWriter::new();
        len.write_var_i32(MAX_PACKET_LEN as i32 + 1);
        server.write_all(&len.into_inner()).unwrap();
        assert_eq!(
            client.read_frame().unwrap_err(),
            ProtocolError::InvalidData("packet length exceeds 2097152")
        );
    }

    #[test]
    fn read_var_i32_prefix_rejects_too_long_varint() {
        assert_eq!(
            read_var_i32_prefix(&[0x80, 0x80, 0x80, 0x80, 0x80]).unwrap_err(),
            ProtocolError::VarIntTooLong
        );
    }
}

// ─── EncryptionResponse (login state, packet id 0x01) ───────────────────────

fn encryption_response_frame(enc_secret: Vec<u8>, enc_token: Vec<u8>) -> PacketFrame {
    use crate::io::PacketWriter;
    let mut body = PacketWriter::new();
    body.write_var_i32(enc_secret.len() as i32);
    body.write_bytes(&enc_secret);
    body.write_var_i32(enc_token.len() as i32);
    body.write_bytes(&enc_token);
    PacketFrame::new(0x01, body.into_inner())
}

// ─── Mojang signed-hex server hash ──────────────────────────────────────────

/// Compute the Minecraft server hash for the session-join request.
///
/// SHA-1(server_id_bytes || shared_secret || public_key_der), interpreted as a
/// two's-complement big-endian signed integer, formatted in lowercase hex
/// (no padding; a '-' prefix if negative).
fn minecraft_server_hash(
    server_id: &str,
    shared_secret: &[u8],
    public_key_der: &[u8],
) -> std::result::Result<String, ()> {
    use num_bigint::BigInt;
    use sha1::{Digest, Sha1};

    let mut hasher = Sha1::new();
    hasher.update(server_id.as_bytes());
    hasher.update(shared_secret);
    hasher.update(public_key_der);
    let digest = hasher.finalize();

    // BigInt::from_signed_bytes_be interprets the byte array as two's-complement.
    let big = BigInt::from_signed_bytes_be(&digest);
    Ok(big.to_str_radix(16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minecraft_server_hash_is_nonempty_hex() {
        let hash = minecraft_server_hash("", &[0u8; 16], &[]).unwrap();
        assert!(!hash.is_empty());
        for (i, c) in hash.chars().enumerate() {
            assert!(
                c.is_ascii_hexdigit() || (i == 0 && c == '-'),
                "unexpected char '{c}' in hash '{hash}'"
            );
        }
    }

    /// The wiki.vg reference: server_id="jeb_", shared_secret=all zeros, public_key=empty.
    /// The known hash for "jeb_" (with empty secret + key) is computed here.
    /// We just verify the function is deterministic and not panicking.
    #[test]
    fn minecraft_server_hash_is_deterministic() {
        let h1 = minecraft_server_hash("test", &[1u8; 16], &[2u8; 32]).unwrap();
        let h2 = minecraft_server_hash("test", &[1u8; 16], &[2u8; 32]).unwrap();
        assert_eq!(h1, h2);
    }
}
