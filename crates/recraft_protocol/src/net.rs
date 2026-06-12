use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::{
    codec::{decode_frame, encode_frame, Compression, PacketFrame},
    io::{ProtocolError, Result},
    v1_8_9::{
        packets::{ClientboundLoginPacket, ClientboundPlayPacket, NextState, ServerboundPacket},
        PROTOCOL_VERSION,
    },
};

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

pub struct BlockingClient {
    stream: TcpStream,
    compression: Compression,
    read_buf: Vec<u8>,
    /// AES-128-CFB8 encryptor enabled after `EncryptionResponse`.
    encryptor: Option<crate::crypto::Aes128Cfb8>,
    /// AES-128-CFB8 decryptor enabled after `EncryptionResponse`.
    decryptor: Option<crate::crypto::Aes128Cfb8>,
}

impl BlockingClient {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            compression: Compression::Disabled,
            read_buf: Vec::new(),
            encryptor: None,
            decryptor: None,
        })
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
                    self.compression = Compression::Enabled { threshold };
                }
                ClientboundLoginPacket::LoginSuccess { uuid, username } => {
                    return Ok(OfflineLoginResult {
                        uuid,
                        username,
                        compression: self.compression,
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
                        compression: self.compression,
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
        self.encryptor = Some(crate::crypto::Aes128Cfb8::encryptor(&shared_secret));
        self.decryptor = Some(crate::crypto::Aes128Cfb8::decryptor(&shared_secret));

        // ── Read post-encryption login packets ────────────────────────────────
        loop {
            let frame = self.read_frame()?;
            match ClientboundLoginPacket::from_frame(frame)? {
                ClientboundLoginPacket::SetCompression { threshold } => {
                    self.compression = Compression::Enabled { threshold };
                }
                ClientboundLoginPacket::LoginSuccess { uuid, username } => {
                    return Ok(OfflineLoginResult {
                        uuid,
                        username,
                        compression: self.compression,
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
        ClientboundPlayPacket::from_frame(self.read_frame()?)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.stream.set_read_timeout(timeout)?;
        Ok(())
    }

    pub fn write_packet(&mut self, frame: PacketFrame) -> Result<()> {
        let mut encoded = encode_frame(&frame, self.compression)?;
        if let Some(enc) = &mut self.encryptor {
            enc.process(&mut encoded);
        }
        self.stream.write_all(&encoded)?;
        Ok(())
    }

    fn read_frame(&mut self) -> Result<PacketFrame> {
        loop {
            if let Some((length, header_len)) = read_var_i32_prefix(&self.read_buf)? {
                if length < 0 {
                    return Err(ProtocolError::InvalidData("negative packet length"));
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
