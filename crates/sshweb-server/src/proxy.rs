//! SOCKS5 入站代理:每台服务器一个本机监听端口,把本地连接经该服务器的 SSH
//! 连接(direct-tcpip 通道)转发到远程内网任意 TCP 服务(数据库 / Web 等)。
//!
//! 与 SFTP 池(坑 14)同思路:共享 SSH 连接**锁外建立 + 连接超时**;断线后下一次
//! 入站连接自动重连。监听端口**只绑定 127.0.0.1**——隧道端口是原生 TCP,无法
//! 复用 HTTP Cookie 认证,loopback 是安全底线(WSL2 下 Windows 宿主仍可经
//! `localhost` 访问)。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::config::ConfigStore;
use crate::ssh;
use crate::utils::Shutdown;
use crate::web::protocol::ServerConfig;

/// 自动分配端口的起始值(配置页未指定端口时从这里起探测空闲端口)。
const PROXY_PORT_START: u16 = 10801;
/// 建立 SSH 连接的超时(与 SFTP 池共用 `ssh::SSH_CONNECT_TIMEOUT`)。
const CONNECT_TIMEOUT: Duration = crate::ssh::SSH_CONNECT_TIMEOUT;
/// SOCKS5 握手(版本协商 + CONNECT 请求)超时:防慢客户端长期占用连接任务。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一台服务器的 SOCKS5 隧道运行时状态。
struct Socks5Proxy {
    /// 实际监听端口。
    port: u16,
    /// 目标服务器配置(已含 `resolve_auth` 解析出的私钥)。
    server: ServerConfig,
    /// 加密配置(host-key TOFU 校验)。
    config: Option<Arc<ConfigStore>>,
    /// 共享 SSH 连接(锁外建立,见 [`Self::get_connection`]);断线后置 None 重连。
    ssh: Mutex<Option<Arc<russh::client::Handle<ssh::SshHandler>>>>,
    /// 停止信号(通知监听循环退出)。
    shutdown: Shutdown,
}

impl Socks5Proxy {
    /// 复用或建立到目标服务器的 SSH 连接。
    ///
    /// 锁外建连(慢连接不阻塞其它入站连接),建好后回填缓存。连接断线后由
    /// [`Self::invalidate_connection`] 置空,下一次调用重新建立。
    async fn get_connection(&self) -> Result<Arc<russh::client::Handle<ssh::SshHandler>>> {
        {
            let guard = self.ssh.lock().await;
            if let Some(handle) = guard.as_ref() {
                return Ok(Arc::clone(handle));
            }
        }
        let handle = Arc::new(
            tokio::time::timeout(
                CONNECT_TIMEOUT,
                ssh::connect(&self.server, self.config.as_deref()),
            )
            .await
            .map_err(|_| anyhow::anyhow!("SSH 连接超时({CONNECT_TIMEOUT:?})"))??,
        );
        let mut guard = self.ssh.lock().await;
        if guard.is_none() {
            *guard = Some(handle);
        }
        // 并发建连时后到者复用缓存中的连接、丢弃自己新建的那条,避免瞬时双连接。
        let cached = guard
            .as_ref()
            .context("ssh connection lost after connect")?;
        Ok(Arc::clone(cached))
    }

    /// 丢弃缓存的 SSH 连接(转发失败后触发,下一次入站连接重新建立)。
    async fn invalidate_connection(&self) {
        *self.ssh.lock().await = None;
    }
}

/// 一个运行中的 SOCKS5 隧道(供 REST 查询)。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    /// 服务器标识(`user@host:port`,与前端 `serverTargetKey` 一致)。
    pub server_key: String,
    /// 服务器显示名。
    pub name: String,
    /// 本地监听端口。
    pub port: u16,
}

/// 全局 SOCKS5 隧道注册表(挂 `ServerState`):每台服务器一个代理端口。
#[derive(Clone)]
pub struct ProxyRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<Socks5Proxy>>>>,
    config: Option<Arc<ConfigStore>>,
}

impl Default for ProxyRegistry {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ProxyRegistry {
    /// 创建空注册表。
    pub fn new(config: Option<Arc<ConfigStore>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// 服务器标识(`user@host:port`),与前端 `serverTargetKey` 保持一致。
    pub fn server_key(server: &ServerConfig) -> String {
        server.target_key()
    }

    /// 开启某服务器的 SOCKS5 隧道。`port` 为 0 时从 `PROXY_PORT_START` 起自动
    /// 分配;已开启时幂等返回现有状态。绑定失败(端口被占)返回错误。
    pub async fn start(&self, server: ServerConfig, port: u16) -> Result<ProxyStatus> {
        let server_key = Self::server_key(&server);
        {
            let guard = self.inner.lock().await;
            if let Some(existing) = guard.get(&server_key) {
                return Ok(ProxyStatus {
                    server_key,
                    name: existing.server.name.clone(),
                    port: existing.port,
                });
            }
        }

        let listener = self.bind_listener(port).await?;
        let actual_port = listener
            .local_addr()
            .map(|a| a.port())
            .context("socks5 listener addr")?;
        let name = server.name.clone();
        let proxy = Arc::new(Socks5Proxy {
            port: actual_port,
            server,
            config: self.config.clone(),
            ssh: Mutex::new(None),
            shutdown: Shutdown::new(),
        });
        self.inner
            .lock()
            .await
            .insert(server_key.clone(), Arc::clone(&proxy));

        let task_proxy = Arc::clone(&proxy);
        tokio::spawn(async move { run_listener(listener, task_proxy).await });
        debug!(%server_key, port = %actual_port, "socks5 tunnel started");
        Ok(ProxyStatus {
            server_key,
            name,
            port: actual_port,
        })
    }

    /// 绑定 `127.0.0.1:port`;`port == 0` 时从 `PROXY_PORT_START`
    /// 起探测空闲端口。
    async fn bind_listener(&self, port: u16) -> Result<TcpListener> {
        if port != 0 {
            // 指定端口:先给出友好的占用错误。
            {
                let guard = self.inner.lock().await;
                if guard.values().any(|p| p.port == port) {
                    bail!("本地端口 {port} 已被其它隧道占用");
                }
            }
            return TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("无法监听 127.0.0.1:{port}"));
        }
        for p in PROXY_PORT_START..=u16::MAX {
            match TcpListener::bind(("127.0.0.1", p)).await {
                Ok(listener) => return Ok(listener),
                Err(_) => continue, // 被本进程其它隧道或系统占用,尝试下一个
            }
        }
        bail!("无法找到可用的本地端口(从 {PROXY_PORT_START} 起)")
    }

    /// 停止某服务器的隧道(关闭监听与 SSH 连接)。返回是否曾运行。
    pub async fn stop(&self, server_key: &str) -> bool {
        let proxy = self.inner.lock().await.remove(server_key);
        if let Some(proxy) = proxy {
            proxy.shutdown.shutdown();
            proxy.invalidate_connection().await;
            debug!(%server_key, port = %proxy.port, "socks5 tunnel stopped");
            true
        } else {
            false
        }
    }

    /// 当前所有运行中的隧道(按端口排序)。
    pub async fn list(&self) -> Vec<ProxyStatus> {
        let guard = self.inner.lock().await;
        let mut out: Vec<ProxyStatus> = guard
            .iter()
            .map(|(server_key, proxy)| ProxyStatus {
                server_key: server_key.clone(),
                name: proxy.server.name.clone(),
                port: proxy.port,
            })
            .collect();
        out.sort_by(|a, b| a.port.cmp(&b.port));
        out
    }

    /// 停止全部隧道(服务关闭时调用)。
    pub async fn shutdown_all(&self) {
        let proxies: Vec<Arc<Socks5Proxy>> = {
            let mut guard = self.inner.lock().await;
            guard.drain().map(|(_, proxy)| proxy).collect()
        };
        for proxy in proxies {
            proxy.shutdown.shutdown();
            proxy.invalidate_connection().await;
        }
    }
}

/// 运行一个服务器的 SOCKS5 监听循环:接受本地连接,每连接独立任务转发。
async fn run_listener(listener: TcpListener, proxy: Arc<Socks5Proxy>) {
    loop {
        tokio::select! {
            _ = proxy.shutdown.wait() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let task = Arc::clone(&proxy);
                        tokio::spawn(async move {
                            if let Err(err) = handle_connection(&task, stream).await {
                                debug!(port = %task.port, ?err, "socks5 connection closed");
                            }
                        });
                    }
                    Err(err) => {
                        warn!(port = %proxy.port, ?err, "socks5 listener error, stopping");
                        break;
                    }
                }
            }
        }
    }
}

/// 处理一个 SOCKS5 入站连接:握手 → 解析目标 → 打开 direct-tcpip 通道 → 双向
/// 转发。通道打开成功后才回 success 回复,失败回 general failure。
async fn handle_connection(proxy: &Socks5Proxy, mut stream: TcpStream) -> Result<()> {
    // 认证凭据:用户名非空 → 要求 RFC 1929 用户名/密码认证;否则 no-auth。
    let auth = proxy
        .server
        .socks5_tunnel
        .as_ref()
        .filter(|t| !t.username.is_empty())
        .map(|t| (t.username.as_str(), t.password.as_str()));
    let (host, port) = tokio::time::timeout(HANDSHAKE_TIMEOUT, socks5_handshake(&mut stream, auth))
        .await
        .map_err(|_| anyhow::anyhow!("SOCKS5 handshake timeout"))??;
    let target = match open_target_with_retry(proxy, &host, port).await {
        Ok(target) => target,
        Err(err) => {
            // SOCKS5 reply: general failure。
            let _ = stream
                .write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
            bail!("socks5 connect to {host}:{port} failed: {err:#}");
        }
    };
    // SOCKS5 reply: success, BND.ADDR = 0.0.0.0:0。
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let mut target = target;
    let mut local = stream;
    tokio::io::copy_bidirectional(&mut local, &mut target).await?;
    Ok(())
}

/// 打开 direct-tcpip 通道;失败一次(连接可能已断)则丢弃缓存连接重连一次。
async fn open_target_with_retry(
    proxy: &Socks5Proxy,
    host: &str,
    port: u16,
) -> Result<impl AsyncRead + AsyncWrite + Send + Unpin> {
    match open_target_once(proxy, host, port).await {
        Ok(target) => Ok(target),
        Err(first) => {
            proxy.invalidate_connection().await;
            match open_target_once(proxy, host, port).await {
                Ok(target) => Ok(target),
                Err(second) => bail!("{first:#}; retry: {second:#}"),
            }
        }
    }
}

/// 用共享 SSH 连接打开到远程目标的 direct-tcpip 通道。
async fn open_target_once(
    proxy: &Socks5Proxy,
    host: &str,
    port: u16,
) -> Result<impl AsyncRead + AsyncWrite + Send + Unpin> {
    // 复用/建立共享 SSH 连接(lazy 建连,缓存复用)。`get_connection` 返回的
    // Arc 独立持有该连接,因此可以在**锁外**打开 direct-tcpip 通道——避免持锁
    // await 网络 I/O 阻塞其它入站连接的缓存命中与断线重连(同坑 14 的锁外语义)。
    let handle = proxy.get_connection().await?;
    ssh::open_target(&*handle, host, port).await
}

/// 完成 SOCKS5 版本协商(no-auth 或 RFC 1929 用户名/密码)与 CONNECT 请求解析;
/// 目标任意 host:port。`auth = Some((user, pass))` 时要求认证,`None` 时
/// no-auth。
async fn socks5_handshake(
    stream: &mut TcpStream,
    auth: Option<(&str, &str)>,
) -> Result<(String, u16)> {
    // 版本协商:VER + NMETHODS + METHODS。
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 {
        bail!("SOCKS5 unsupported version {}", header[0]);
    }
    let nmethods = header[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    match auth {
        Some((username, password)) => {
            // 配置了用户名/密码:要求 RFC 1929 认证(方法 0x02)。
            if !methods.contains(&0x02) {
                stream.write_all(&[0x05, 0xff]).await?;
                bail!("SOCKS5 client does not support username/password authentication");
            }
            stream.write_all(&[0x05, 0x02]).await?;
            // RFC 1929 子协商:VER + ULEN + UNAME + PLEN + PASSWD。
            let mut ver = [0u8; 1];
            stream.read_exact(&mut ver).await?;
            if ver[0] != 0x01 {
                bail!("SOCKS5 bad username/password version {}", ver[0]);
            }
            let mut ulen = [0u8; 1];
            stream.read_exact(&mut ulen).await?;
            let mut uname = vec![0u8; ulen[0] as usize];
            stream.read_exact(&mut uname).await?;
            let mut plen = [0u8; 1];
            stream.read_exact(&mut plen).await?;
            let mut pwd = vec![0u8; plen[0] as usize];
            stream.read_exact(&mut pwd).await?;
            if uname.as_slice() == username.as_bytes() && pwd.as_slice() == password.as_bytes() {
                stream.write_all(&[0x01, 0x00]).await?;
            } else {
                stream.write_all(&[0x01, 0x01]).await?;
                bail!("SOCKS5 username/password authentication failed");
            }
        }
        None => {
            if !methods.contains(&0x00) {
                stream.write_all(&[0x05, 0xff]).await?;
                bail!("SOCKS5 requires authentication (only no-auth is supported)");
            }
            stream.write_all(&[0x05, 0x00]).await?;
        }
    }

    // CONNECT 请求:VER + CMD + RSV + ATYP + ADDR + PORT。
    let mut req = [0u8; 4];
    stream.read_exact(&mut req).await?;
    if req[1] != 0x01 {
        // REP=0x07: command not supported。
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        bail!("SOCKS5 only supports CONNECT");
    }
    let host = match req[3] {
        0x01 => {
            let mut b = [0u8; 4];
            stream.read_exact(&mut b).await?;
            format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut h = vec![0u8; len[0] as usize];
            stream.read_exact(&mut h).await?;
            String::from_utf8_lossy(&h).into_owned()
        }
        0x04 => {
            let mut b = [0u8; 16];
            stream.read_exact(&mut b).await?;
            let mut out = String::new();
            for (i, chunk) in b.chunks_exact(2).enumerate() {
                if i > 0 {
                    out.push(':');
                }
                out.push_str(&format!("{:02x}{:02x}", chunk[0], chunk[1]));
            }
            out
        }
        _ => {
            // REP=0x08: address type not supported。
            stream
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;
            bail!("SOCKS5 unknown address type {}", req[3]);
        }
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    Ok((host, u16::from_be_bytes(port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 认证成功:声明 user/pass → 服务端回 0x05 0x02 → 子协商成功 0x01 0x00 →
    /// CONNECT 域名解析为 `(host, port)`。
    #[tokio::test]
    async fn socks5_handshake_auth_ok() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (host, port) = socks5_handshake(&mut stream, Some(("user", "pass")))
                .await
                .unwrap();
            assert_eq!(host, "example.com");
            assert_eq!(port, 80);
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x02]);

        client
            .write_all(&[
                0x01, 0x04, b'u', b's', b'e', b'r', 0x04, b'p', b'a', b's', b's',
            ])
            .await
            .unwrap();
        let mut auth_resp = [0u8; 2];
        client.read_exact(&mut auth_resp).await.unwrap();
        assert_eq!(auth_resp, [0x01, 0x00]);

        client
            .write_all(&[0x05, 0x01, 0x00, 0x03, 11])
            .await
            .unwrap();
        client.write_all(b"example.com").await.unwrap();
        client.write_all(&[0x00, 80]).await.unwrap();

        server.await.unwrap();
    }

    /// 认证失败:密码错误 → 服务端回 0x01 0x01 并断开。
    #[tokio::test]
    async fn socks5_handshake_auth_fail() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let err = socks5_handshake(&mut stream, Some(("user", "pass")))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("authentication failed"));
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x02]);

        client
            .write_all(&[
                0x01, 0x04, b'u', b's', b'e', b'r', 0x04, b'w', b'r', b'o', b'n', b'g',
            ])
            .await
            .unwrap();
        let mut auth_resp = [0u8; 2];
        client.read_exact(&mut auth_resp).await.unwrap();
        assert_eq!(auth_resp, [0x01, 0x01]);

        server.await.unwrap();
    }
}
