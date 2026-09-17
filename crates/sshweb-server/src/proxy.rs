//! SOCKS5 入站代理:每台服务器(以及「本机」)一个本机监听端口。远程服务器把
//! 本地连接经该服务器的 SSH 连接(direct-tcpip 通道)转发到远程内网任意 TCP
//! 服务(数据库 / Web 等);「本机」条目则是**直连**——由 sshweb 主机自己解析并
//! 建立 TCP 连接,不经过任何 SSH。
//!
//! 与 SFTP 池(坑 14)同思路:共享 SSH 连接**锁外建立 + 连接超时**;断线后下一次
//! 入站连接自动重连。监听端口**只绑定 127.0.0.1**——隧道端口是原生 TCP,无法
//! 复用 HTTP Cookie 认证,loopback 是安全底线(WSL2 下 Windows 宿主仍可经
//! `localhost` 访问)。直连代理同理,且**不要**加上绑定地址选项:那一个字段就会
//! 把它变成局域网开放代理(已知坑 83)。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::config::ConfigStore;
use crate::ssh::{self, ByteStream};
use crate::utils::Shutdown;
use crate::web::protocol::{ServerConfig, Socks5Tunnel};

/// 自动分配端口的起始值(配置页未指定端口时从这里起探测空闲端口)。
const PROXY_PORT_START: u16 = 10801;
/// 建立 SSH 连接的超时(与 SFTP 池共用 `ssh::SSH_CONNECT_TIMEOUT`)。
const CONNECT_TIMEOUT: Duration = crate::ssh::SSH_CONNECT_TIMEOUT;
/// SOCKS5 握手(版本协商 + CONNECT 请求)超时:防慢客户端长期占用连接任务。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 「本机」直连代理在注册表里的键。镜像前端 `serverTargetKey(null) === "local"`
/// ——本机代理与本机终端/文件浏览器必须用同一个身份键。**不可能**与服务器冲突:
/// [`ServerConfig::target_key`] 一定含 `@` 与 `:`。
pub const LOCAL_PROXY_KEY: &str = "local";

/// 「本机」直连代理的显示名(服务端无 i18n)。`ProxyStatus.name` 的唯一消费者是
/// 前端的 toast;前端要显示本机名称时用 `t($lang,"servers.local")`,不要直接
/// 渲染本字段。
const LOCAL_PROXY_NAME: &str = "本机";

/// 转发目标的字节流。两支的具体类型不同(russh 的 direct-tcpip 通道流 vs 原生
/// `TcpStream`),`impl Trait` 无法在两个 `match` 分支间统一,只能装箱;装箱后仍
/// 满足 `copy_bidirectional` 所需的 `AsyncRead + AsyncWrite + Unpin`(复用
/// `ssh.rs` 的 [`ByteStream`],它就是这个形状的既有 trait)。
type TargetStream = Box<dyn ByteStream>;

/// 一条代理的上游:远程 SSH 隧道,或本机直连。
enum ProxyUpstream {
    /// 经该服务器的共享 SSH 连接(direct-tcpip)转发到远程内网。
    Ssh(Box<ServerConfig>),
    /// 本机直连:sshweb 主机自己解析域名并建 TCP 连接,不经过 SSH。
    Direct,
}

impl ProxyUpstream {
    /// 该上游的服务器配置(仅 SSH 上游有)。
    fn server(&self) -> Option<&ServerConfig> {
        match self {
            Self::Ssh(server) => Some(server),
            Self::Direct => None,
        }
    }
}

/// 从隧道/代理配置取出入站认证:用户名非空才要求 RFC 1929 认证。
fn proxy_auth(tunnel: Option<&Socks5Tunnel>) -> Option<(String, String)> {
    tunnel
        .filter(|tunnel| !tunnel.username.is_empty())
        .map(|tunnel| (tunnel.username.clone(), tunnel.password.clone()))
}

/// 一个运行中的 SOCKS5 代理。
struct Socks5Proxy {
    /// 实际监听端口。
    port: u16,
    /// 显示名(见 [`LOCAL_PROXY_NAME`])。
    name: String,
    /// 上游转发方式。
    upstream: ProxyUpstream,
    /// 入站认证(空 = no-auth)。
    auth: Option<(String, String)>,
    /// 加密配置(host-key TOFU 校验;仅 SSH 上游使用)。
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
        let server = self
            .upstream
            .server()
            .context("本机直连代理没有可复用的 SSH 连接")?;
        {
            let guard = self.ssh.lock().await;
            if let Some(handle) = guard.as_ref() {
                return Ok(Arc::clone(handle));
            }
        }
        let handle = Arc::new(
            tokio::time::timeout(
                CONNECT_TIMEOUT,
                ssh::connect(server, self.config.as_deref()),
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
        let key = Self::server_key(&server);
        let name = server.name.clone();
        let auth = proxy_auth(server.socks5_tunnel.as_ref());
        self.insert_and_spawn(key, name, ProxyUpstream::Ssh(Box::new(server)), auth, port)
            .await
    }

    /// 开启「本机」直连 SOCKS5 代理(键固定为 [`LOCAL_PROXY_KEY`]):入站的
    /// `CONNECT` 由 sshweb 主机自己解析并直连,不建立任何 SSH 连接。
    /// `tunnel` 只提供入站认证与端口偏好(空用户名 = no-auth,端口 0 =
    /// 自动分配)。
    pub async fn start_direct(&self, tunnel: &Socks5Tunnel) -> Result<ProxyStatus> {
        self.insert_and_spawn(
            LOCAL_PROXY_KEY.to_string(),
            LOCAL_PROXY_NAME.to_string(),
            ProxyUpstream::Direct,
            proxy_auth(Some(tunnel)),
            tunnel.port,
        )
        .await
    }

    /// 注册并启动一条代理(已有同键代理时幂等返回其状态)。
    ///
    /// 远程隧道与直连代理共用**同一个注册表**:第二个注册表会让端口分配器打架
    /// ——显式端口的冲突检测与从 10801 起的自动探测都必须看到全部已占端口。
    async fn insert_and_spawn(
        &self,
        key: String,
        name: String,
        upstream: ProxyUpstream,
        auth: Option<(String, String)>,
        port: u16,
    ) -> Result<ProxyStatus> {
        {
            let guard = self.inner.lock().await;
            if let Some(existing) = guard.get(&key) {
                return Ok(ProxyStatus {
                    server_key: key,
                    name: existing.name.clone(),
                    port: existing.port,
                });
            }
        }

        let listener = self.bind_listener(port).await?;
        let actual_port = listener
            .local_addr()
            .map(|a| a.port())
            .context("socks5 listener addr")?;
        let proxy = Arc::new(Socks5Proxy {
            port: actual_port,
            name: name.clone(),
            upstream,
            auth,
            config: self.config.clone(),
            ssh: Mutex::new(None),
            shutdown: Shutdown::new(),
        });
        self.inner
            .lock()
            .await
            .insert(key.clone(), Arc::clone(&proxy));

        let task_proxy = Arc::clone(&proxy);
        tokio::spawn(async move { run_listener(listener, task_proxy).await });
        debug!(%key, port = %actual_port, "socks5 proxy started");
        Ok(ProxyStatus {
            server_key: key,
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
                    bail!("本地端口 {port} 已被其它代理占用");
                }
            }
            return TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("无法监听 127.0.0.1:{port}"));
        }
        for p in PROXY_PORT_START..=u16::MAX {
            match TcpListener::bind(("127.0.0.1", p)).await {
                Ok(listener) => return Ok(listener),
                Err(_) => continue, // 被本进程其它代理或系统占用,尝试下一个
            }
        }
        bail!("无法找到可用的本地端口(从 {PROXY_PORT_START} 起)")
    }

    /// 停止某条代理(关闭监听与 SSH 连接)。返回是否曾运行。
    pub async fn stop(&self, server_key: &str) -> bool {
        let proxy = self.inner.lock().await.remove(server_key);
        if let Some(proxy) = proxy {
            proxy.shutdown.shutdown();
            proxy.invalidate_connection().await;
            debug!(%server_key, port = %proxy.port, "socks5 proxy stopped");
            true
        } else {
            false
        }
    }

    /// 当前所有运行中的代理(按端口排序)。
    pub async fn list(&self) -> Vec<ProxyStatus> {
        let guard = self.inner.lock().await;
        let mut out: Vec<ProxyStatus> = guard
            .iter()
            .map(|(server_key, proxy)| ProxyStatus {
                server_key: server_key.clone(),
                name: proxy.name.clone(),
                port: proxy.port,
            })
            .collect();
        out.sort_by(|a, b| a.port.cmp(&b.port));
        out
    }

    /// 停止全部代理(服务关闭时调用)。
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

/// 处理一个 SOCKS5 入站连接:握手 → 解析目标 → 打开上游连接 → 双向转发。
/// 上游连接成功后才回 success 回复,失败回 general failure。
async fn handle_connection(proxy: &Socks5Proxy, mut stream: TcpStream) -> Result<()> {
    // 认证凭据:用户名非空 → 要求 RFC 1929 用户名/密码认证;否则 no-auth。
    let auth = proxy
        .auth
        .as_ref()
        .map(|(username, password)| (username.as_str(), password.as_str()));
    let (host, port) = tokio::time::timeout(HANDSHAKE_TIMEOUT, socks5_handshake(&mut stream, auth))
        .await
        .map_err(|_| anyhow::anyhow!("SOCKS5 handshake timeout"))??;
    let mut target = match open_target(proxy, &host, port).await {
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
    let mut local = stream;
    tokio::io::copy_bidirectional(&mut local, &mut target).await?;
    Ok(())
}

/// 打开到 `host:port` 的上游连接,按上游分支:
///
/// - `Ssh`:经 direct-tcpip 通道转发;失败一次(连接可能已断)则丢弃缓存连接
///   重连一次。**域名由远程主机解析**。
/// - `Direct`:sshweb 主机自己 `TcpStream::connect`(超时与 SSH 建连共用
///   `CONNECT_TIMEOUT`),失败不重试也无连接可失效。**域名由 sshweb 主机解析**
///   ——这正是直连代理的意义:访问 sshweb 能访问、而远程内网访问不到的服务。
async fn open_target(proxy: &Socks5Proxy, host: &str, port: u16) -> Result<TargetStream> {
    match &proxy.upstream {
        ProxyUpstream::Ssh(_) => match open_ssh_target_once(proxy, host, port).await {
            Ok(target) => Ok(Box::new(target)),
            Err(first) => {
                proxy.invalidate_connection().await;
                match open_ssh_target_once(proxy, host, port).await {
                    Ok(target) => Ok(Box::new(target)),
                    Err(second) => bail!("{first:#}; retry: {second:#}"),
                }
            }
        },
        ProxyUpstream::Direct => {
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
                .await
                .map_err(|_| anyhow::anyhow!("连接 {host}:{port} 超时({CONNECT_TIMEOUT:?})"))?
                .with_context(|| format!("无法连接 {host}:{port}"))?;
            Ok(Box::new(stream))
        }
    }
}

/// 用共享 SSH 连接打开到远程目标的 direct-tcpip 通道。
async fn open_ssh_target_once(proxy: &Socks5Proxy, host: &str, port: u16) -> Result<TargetStream> {
    // 复用/建立共享 SSH 连接(lazy 建连,缓存复用)。`get_connection` 返回的
    // Arc 独立持有该连接,因此可以在**锁外**打开 direct-tcpip 通道——避免持锁
    // await 网络 I/O 阻塞其它入站连接的缓存命中与断线重连(同坑 14 的锁外语义)。
    let handle = proxy.get_connection().await?;
    Ok(Box::new(ssh::open_target(&*handle, host, port).await?))
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

    /// 一个回环 echo 服务:原样回写收到的字节(直连代理的转发目标)。
    async fn spawn_echo() -> std::net::SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if stream.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        addr
    }

    /// 空隧道 = no-auth + 自动分配端口。
    fn empty_tunnel() -> Socks5Tunnel {
        Socks5Tunnel {
            port: 0,
            username: String::new(),
            password: String::new(),
        }
    }

    /// 以 SOCKS5 客户端身份走完 no-auth 握手并 CONNECT 到 `target`,`payload`
    /// 原样往返。`host` 决定 ATYP 形式:含 `.` 的按 IPv4 发,否则按域名发
    /// (`0x03`——那条路径证明域名由 **sshweb 主机**解析,这正是直连代理的意义)。
    async fn socks5_round_trip(proxy_port: u16, host: &str, target: std::net::SocketAddr) {
        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        // 版本协商:只提供 no-auth。
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x00], "no-auth must be accepted");

        // CONNECT 请求。
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        if host.contains('.') {
            let octets: Vec<u8> = host.split('.').map(|p| p.parse().unwrap()).collect();
            assert_eq!(octets.len(), 4);
            client.write_all(&[0x01]).await.unwrap();
            client.write_all(&octets).await.unwrap();
        } else {
            client.write_all(&[0x03, host.len() as u8]).await.unwrap();
            client.write_all(host.as_bytes()).await.unwrap();
        }
        client
            .write_all(&target.port().to_be_bytes())
            .await
            .unwrap();

        // 回复:VER REP RSV ATYP BND.ADDR BND.PORT(IPv4 形式共 10 字节)。
        let mut reply = [0u8; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..2], &[0x05, 0x00], "CONNECT must succeed");

        // 字节往返。
        client.write_all(b"hello socks5").await.unwrap();
        let mut echoed = [0u8; 12];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"hello socks5");
    }

    /// 直连代理(本机 SOCKS5)端到端:no-auth 握手 → CONNECT → 字节往返,
    /// IPv4 与域名两种地址形式都覆盖。
    #[tokio::test]
    async fn direct_proxy_forwards_bytes() {
        let echo = spawn_echo().await;
        let registry = ProxyRegistry::default();
        let status = registry.start_direct(&empty_tunnel()).await.unwrap();
        assert_eq!(status.server_key, LOCAL_PROXY_KEY);
        assert_eq!(status.name, LOCAL_PROXY_NAME);
        // 端口 0 → 从 PROXY_PORT_START 起自动分配(绝不复用 1080)。
        assert!(status.port >= PROXY_PORT_START);

        socks5_round_trip(status.port, "127.0.0.1", echo).await;
        socks5_round_trip(status.port, "localhost", echo).await;

        // 列表与停止都按 key 工作(直连复用同一套 REST 语义)。
        let listed = registry.list().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].server_key, LOCAL_PROXY_KEY);
        assert!(registry.stop(LOCAL_PROXY_KEY).await);
        assert!(registry.list().await.is_empty());
    }

    /// 配了用户名密码就必须走 RFC 1929:只提供 no-auth 的客户端被拒(0x05 0xff)。
    #[tokio::test]
    async fn direct_proxy_enforces_configured_auth() {
        let registry = ProxyRegistry::default();
        let status = registry
            .start_direct(&Socks5Tunnel {
                port: 0,
                username: "user".into(),
                password: "pass".into(),
            })
            .await
            .unwrap();

        let mut client = TcpStream::connect(("127.0.0.1", status.port))
            .await
            .unwrap();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0xff]);

        registry.stop(LOCAL_PROXY_KEY).await;
    }

    /// 同一键再次 start 是幂等的:返回同一个端口,不会起第二个监听。
    #[tokio::test]
    async fn direct_proxy_start_is_idempotent() {
        let registry = ProxyRegistry::default();
        let first = registry.start_direct(&empty_tunnel()).await.unwrap();
        let second = registry.start_direct(&empty_tunnel()).await.unwrap();
        assert_eq!(first.port, second.port);
        assert_eq!(registry.list().await.len(), 1);
        registry.stop(LOCAL_PROXY_KEY).await;
    }
}
