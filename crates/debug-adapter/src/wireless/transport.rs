//! EmberMobile v2 framing and pinned QUIC, based on ember-v2's wire/client modules.
use crate::{error, Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

pub(super) const AUTH: u8 = 1;
pub(super) const RPC: u8 = 2;
pub(super) const UPLOAD: u8 = 4;
pub(super) const MEMORY_READ: u8 = 6;
pub(super) const PAIR: u8 = 11;
pub(super) const FORGET: u8 = 12;
pub(super) const OK: u8 = 128;
pub(super) const DATA: u8 = 129;
pub(super) const END: u8 = 130;
const ERROR: u8 = 255;
const MAX_FRAME: usize = 65536 + 128;

pub(super) async fn read(input: &mut (impl AsyncRead + Unpin)) -> Result<(u8, Vec<u8>)> {
    let mut header = [0; 12];
    input.read_exact(&mut header).await.map_err(error)?;
    if &header[..4] != b"EMB2" || header[5..8] != [0, 0, 0] {
        return Err(Error("无效的 Ember v2 帧头".into()));
    }
    let size = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if size > MAX_FRAME {
        return Err(Error("Ember v2 帧超出大小限制".into()));
    }
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes).await.map_err(error)?;
    Ok((header[4], bytes))
}

pub(super) async fn write(
    output: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    bytes: &[u8],
) -> Result<()> {
    if bytes.len() > MAX_FRAME {
        return Err(Error("Ember v2 帧超出大小限制".into()));
    }
    let mut frame = Vec::with_capacity(12 + bytes.len());
    frame.extend_from_slice(b"EMB2");
    frame.extend_from_slice(&[kind, 0, 0, 0]);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    output.write_all(&frame).await.map_err(error)
}

pub(super) async fn response(recv: &mut quinn::RecvStream, wait: Duration) -> Result<Vec<u8>> {
    response_kind(recv, wait, OK).await
}

async fn response_kind(
    recv: &mut quinn::RecvStream,
    wait: Duration,
    expected: u8,
) -> Result<Vec<u8>> {
    let (kind, bytes) = timeout(wait, read(recv)).await.map_err(error)??;
    if kind == ERROR {
        let fault: Value = serde_json::from_slice(&bytes).map_err(error)?;
        return Err(Error(format!(
            "{}: {}",
            fault["code"].as_str().unwrap_or("remote_error"),
            fault["message"].as_str().unwrap_or("无线调试请求失败")
        )));
    }
    if kind != expected {
        return Err(Error(format!("无效的 Ember v2 响应类型：{kind}")));
    }
    Ok(bytes)
}

pub(super) struct Client {
    pub connection: quinn::Connection,
    endpoint: quinn::Endpoint,
}

impl Client {
    async fn transport(mut address: SocketAddr, advertisement: &Value, pin: &str) -> Result<Self> {
        let certificate = STANDARD
            .decode(
                advertisement["certificate_der"]
                    .as_str()
                    .ok_or_else(|| Error("手机未提供证书".into()))?,
            )
            .map_err(error)?;
        if pin.len() != 64 || format!("{:x}", Sha256::digest(&certificate)) != pin {
            return Err(Error(
                "手机证书指纹与保存的配对不一致，请重新核对设备".into(),
            ));
        }
        let port = advertisement["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| Error("无效的 QUIC 端口".into()))?;
        address.set_port(port);
        let mut roots = quinn::rustls::RootCertStore::empty();
        roots
            .add(quinn::rustls::pki_types::CertificateDer::from(certificate))
            .map_err(error)?;
        let mut tls = quinn::rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"ember/2".to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(error)?,
        ));
        let mut transport = quinn::TransportConfig::default();
        transport.send_window(256 * 1024);
        transport.stream_receive_window((256u32 * 1024).into());
        transport.receive_window((1024u32 * 1024).into());
        transport.keep_alive_interval(Some(Duration::from_secs(5)));
        config.transport_config(Arc::new(transport));
        let bind = if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = std::net::UdpSocket::bind(bind).map_err(error)?;
        socket.set_nonblocking(true).map_err(error)?;
        let options = socket2::SockRef::from(&socket);
        options
            .set_recv_buffer_size(2 * 1024 * 1024)
            .map_err(error)?;
        options.set_send_buffer_size(64 * 1024).map_err(error)?;
        let mut endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .map_err(error)?;
        endpoint.set_default_client_config(config);
        let connection = timeout(
            Duration::from_secs(5),
            endpoint.connect(address, "ember.local").map_err(error)?,
        )
        .await
        .map_err(error)?
        .map_err(error)?;
        Ok(Self {
            connection,
            endpoint,
        })
    }

    pub async fn connect(address: SocketAddr, advertisement: &Value, token: &str) -> Result<Self> {
        let (_, pin) = token
            .rsplit_once('.')
            .ok_or_else(|| Error("无效的无线配对凭据".into()))?;
        let client = Self::transport(address, advertisement, pin).await?;
        let auth = if advertisement["traffic_control"].as_u64().unwrap_or(0) >= 2 {
            serde_json::to_vec(&json!({"token":token,"control":true})).map_err(error)?
        } else {
            token.as_bytes().to_vec()
        };
        client.exchange(AUTH, &auth).await?;
        Ok(client)
    }

    pub async fn pair(
        address: SocketAddr,
        advertisement: &Value,
        name: &str,
        on_code: &mut dyn FnMut(&str),
    ) -> Result<(Value, String)> {
        if advertisement["device"]["pairing"] != 1 {
            return Err(Error("请升级手机 App 以支持长期配对".into()));
        }
        let pin = advertisement["fingerprint"]
            .as_str()
            .ok_or_else(|| Error("手机未提供证书指纹".into()))?;
        let client = Self::transport(address, advertisement, pin).await?;
        let mut random = [0u8; 32];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| Error("无法生成配对密钥".into()))?;
        let secret: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let mut code = [0; 4];
        client
            .connection
            .export_keying_material(&mut code, b"ember-pairing-v1", b"")
            .map_err(|_| Error("无法生成 TLS 配对校验码".into()))?;
        let (mut send, mut recv) = client.connection.open_bi().await.map_err(error)?;
        write(
            &mut send,
            PAIR,
            &serde_json::to_vec(&json!({"name":name,"secret":secret})).map_err(error)?,
        )
        .await?;
        send.finish().map_err(error)?;
        on_code(&format!("{:08}", u32::from_be_bytes(code) % 100_000_000));
        let result: Value =
            serde_json::from_slice(&response(&mut recv, Duration::from_secs(125)).await?)
                .map_err(error)?;
        if result["server_id"] != advertisement["device"]["id"] {
            return Err(Error("配对期间手机身份发生变化".into()));
        }
        let id = result["pair_id"]
            .as_str()
            .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| Error("手机返回无效的配对 ID".into()))?;
        let token = format!("pair:{id}:{secret}.{pin}");
        client.close().await;
        Ok((result, token))
    }

    pub async fn exchange(&self, kind: u8, bytes: &[u8]) -> Result<Vec<u8>> {
        timeout(Duration::from_secs(30), async {
            let (mut send, mut recv) = self.connection.open_bi().await.map_err(error)?;
            write(&mut send, kind, bytes).await?;
            send.finish().map_err(error)?;
            response(&mut recv, Duration::from_secs(30)).await
        })
        .await
        .map_err(error)?
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let payload =
            serde_json::to_vec(&json!({"method":method,"params":params})).map_err(error)?;
        serde_json::from_slice(&self.exchange(RPC, &payload).await?).map_err(error)
    }

    pub async fn read_memory(
        &self,
        session_id: i64,
        core: u32,
        blocks: &[nya_domain::waveform::ReadBlock],
    ) -> Result<Vec<Vec<u8>>> {
        // A bounded sweep keeps debugger commands responsive on the shared worker.
        // Dropping a timed-out stream cancels only this read, not the connection.
        timeout(Duration::from_millis(900), async {
            let mut result = Vec::with_capacity(blocks.len());
            for block in blocks {
                let (mut send, mut recv) = self.connection.open_bi().await.map_err(error)?;
                write(&mut send, MEMORY_READ, &serde_json::to_vec(&json!({
                    "session_id":session_id,"core":core,"address":block.address,"length":block.byte_size
                })).map_err(error)?).await?;
                send.finish().map_err(error)?;
                // Only the outer sweep timer owns the deadline and its message.
                let bytes = response_kind(&mut recv, Duration::from_secs(30), DATA).await?;
                if bytes.len() != block.byte_size {
                    return Err(Error("手机返回不完整的内存数据，已丢弃本轮采集".into()));
                }
                result.push(bytes);
            }
            Ok(result)
        }).await.map_err(|_| Error("无线内存读取超时，请检查网络或减少不连续地址".into()))?
    }

    pub async fn upload(
        &self,
        id: u64,
        bytes: &[u8],
        mut on_progress: impl FnMut(usize),
    ) -> Result<()> {
        timeout(Duration::from_secs(300), async {
            let (mut send, mut recv) = self.connection.open_bi().await.map_err(error)?;
            send.set_priority(-10).map_err(error)?;
            write(
                &mut send,
                UPLOAD,
                &serde_json::to_vec(&json!({"upload_id":id,"offset":0})).map_err(error)?,
            )
            .await?;
            let mut sent = 0;
            for chunk in bytes.chunks(16384) {
                write(&mut send, DATA, chunk).await?;
                sent += chunk.len();
                on_progress(sent);
            }
            write(&mut send, END, &[]).await?;
            send.finish().map_err(error)?;
            let result: Value =
                serde_json::from_slice(&response(&mut recv, Duration::from_secs(30)).await?)
                    .map_err(error)?;
            if result["received"].as_u64() != Some(bytes.len() as u64) {
                return Err(Error("手机接收的固件长度不一致".into()));
            }
            Ok(())
        })
        .await
        .map_err(error)?
    }

    pub async fn close(&self) {
        self.connection.close(0u32.into(), b"client closed");
        let _ = timeout(Duration::from_millis(500), self.endpoint.wait_idle()).await;
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"client dropped");
    }
}
