use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sshweb_server::{Server, ServerOptions};
use tokio::signal::unix::{signal, SignalKind};
use tracing::{error, info};

/// The sshweb server CLI interface.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Subcommand (export/import config, or run the server).
    #[clap(subcommand)]
    command: Option<Command>,

    /// Specify port to listen on.
    #[clap(long, short = 'p', default_value_t = 2223)]
    port: u16,

    /// Which IP address or network interface to listen on. Defaults to `::`
    /// (dual-stack IPv4+IPv6); use `0.0.0.0` for IPv4 only.
    #[clap(long, value_parser, default_value = "::")]
    listen: IpAddr,

    /// Command used to spawn terminal shells.
    #[clap(long)]
    shell: Option<String>,

    /// Path to the encrypted server configuration file.
    #[clap(long)]
    config: Option<PathBuf>,

    /// PEM TLS certificate; both this and `--tls-key` enable HTTPS serving.
    #[clap(long)]
    tls_cert: Option<PathBuf>,

    /// PEM TLS private key; both this and `--tls-cert` enable HTTPS serving.
    #[clap(long)]
    tls_key: Option<PathBuf>,

    /// How long (seconds) an idle session (no connected browser) is kept
    /// alive before its terminal processes are reaped.
    #[clap(long)]
    session_ttl: Option<u64>,
    /// Run the server in the background, detached from the terminal. Logs are
    /// appended to the `--log` file (default: `sshweb.log` in the working
    /// directory); the launching shell prompt returns immediately.
    #[clap(long, short = 'd')]
    detach: bool,

    /// Log file for a detached (`-d`) server. Only meaningful with `-d`.
    #[clap(long, requires = "detach")]
    log: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Export the whole encrypted configuration (servers + keys, incl.
    /// private keys inside the ciphertext) to stdout or a file.
    Export {
        /// Output file (defaults to stdout). The bytes are the `config.enc`
        /// JSON, so `> backup.enc` / `-o backup.enc` both work.
        #[clap(long, short)]
        output: Option<PathBuf>,
    },
    /// Import a previously-exported encrypted configuration file
    /// (`config.enc` JSON) and replace the current config.
    Import {
        /// The backup file to restore (the `config.enc` JSON exported by
        /// `export`).
        input: PathBuf,
    },
    /// List the saved SSH keys (public parts only; private keys never leave
    /// the encrypted config file). Prompts for the access password.
    Keys,
    /// Change the page access password (prompts for the current and new
    /// password; all settings are preserved and re-encrypted).
    ResetPassword,
    /// Print the configuration store status (path, server/key counts). Does
    /// not require a password.
    Status,
    /// List the configured servers (host, username, auth method; never the
    /// password). Prompts for the access password.
    Servers,
    /// Print the version of the sshweb server.
    Version,
}

#[tokio::main]
async fn start(args: Args) -> Result<()> {
    let addr = SocketAddr::new(args.listen, args.port);

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let mut options = ServerOptions::default();
    options.shell = args.shell;
    options.config_path = args.config;
    options.session_ttl = args.session_ttl;
    options.tls_cert = args.tls_cert.clone();
    options.tls_key = args.tls_key.clone();

    // Warn when serving plaintext on a non-loopback interface without TLS
    // (安全审查 M2): the auth cookie and all terminal content then travel in
    // cleartext over the network.
    if args.tls_cert.is_none() && args.tls_key.is_none() && !(args.listen.is_loopback()) {
        eprintln!(
            "⚠️  警告:正在以明文 HTTP 监听 {} \
             (非回环地址)。页面访问密码与终端内容将明文传输。\n⚠️  建议用 `--tls-cert` / \
             `--tls-key` 启用 HTTPS,或仅监听 127.0.0.1/::1。",
            args.listen
        );
    }

    let server = Server::new(options)?;

    // First-boot: print the one-time setup key so the operator can log in and
    // set the access password. It goes to stdout (the log file under `-d`),
    // never over the network.
    if let Some(key) = server.state().config().setup_key() {
        println!("====================================================");
        println!("首次启动:请在浏览器打开本服务,使用下面的安装密钥登录,");
        println!("登录后将强制设置访问密码。此密钥仅打印一次,请妥善保存。");
        println!("安装密钥: {key}");
        println!("====================================================");
    }

    let serve_task = async {
        info!("server listening at {addr}");
        server.bind(&addr).await
    };

    let signals_task = async {
        tokio::select! {
            Some(()) = sigterm.recv() => (),
            Some(()) = sigint.recv() => (),
            else => return Ok(()),
        }
        info!("gracefully shutting down...");
        server.shutdown();
        Ok(())
    };

    tokio::try_join!(serve_task, signals_task)?;
    Ok(())
}

/// Run an `export` / `import` config subcommand (no server started).
fn run_config_command(args: &Args) -> Result<()> {
    let Some(cmd) = &args.command else {
        return Ok(());
    };
    // The config path is required for export/import; fall back to the server's
    // default path so `sshweb-server export` works without --config.
    let path = args
        .config
        .clone()
        .unwrap_or_else(sshweb_server::config::ConfigStore::default_path);
    let store = sshweb_server::config::ConfigStore::new(path)?;

    match cmd {
        Command::Export { output } => {
            let bytes = store.export_backup()?;
            match output {
                Some(out) => {
                    std::fs::write(out, &bytes)
                        .with_context(|| format!("write export file {}", out.display()))?;
                    println!("导出配置到 {}", out.display());
                }
                None => {
                    // Write raw bytes to stdout (no trailing newline, so the
                    // file can be piped straight into `import`).
                    let mut stdout = std::io::stdout().lock();
                    stdout.write_all(&bytes)?;
                    stdout.flush()?;
                }
            }
        }
        Command::Import { input } => {
            let mut bytes = Vec::new();
            std::fs::File::open(input)
                .with_context(|| format!("open import file {}", input.display()))?
                .read_to_end(&mut bytes)?;
            store.import_backup(&bytes)?;
            println!("已导入配置(重启服务后用备份的访问密码登录)");
        }
        Command::Keys => {
            let password = read_password("访问密码: ")?;
            let settings = store.decrypt(&password)?;
            if settings.keys.is_empty() {
                println!("暂无保存的 SSH 密钥");
            } else {
                println!("{:<20} {:<16} 指纹", "ID", "名称");
                for k in &settings.keys {
                    println!("{:<20} {:<16} {}", k.id, k.name, k.fingerprint);
                }
            }
        }
        Command::ResetPassword => {
            let old = read_password("当前访问密码: ")?;
            let new = read_password("新访问密码: ")?;
            let confirm = read_password("确认新访问密码: ")?;
            if new != confirm {
                anyhow::bail!("两次输入的新密码不一致");
            }
            store.reencrypt(&old, &new)?;
            println!("访问密码已重置(重启服务后用新密码登录)");
        }
        Command::Status => match store.status() {
            Some(st) => {
                println!("配置文件: {}", st.path);
                println!("访问密码: 已设置");
                println!("服务器:   {}", st.server_count);
                println!("密钥:     {}", st.key_count);
            }
            None => {
                println!("配置文件: {}", store.path().display());
                println!("访问密码: 未设置(首次访问网页时设置)");
            }
        },
        Command::Servers => {
            let password = read_password("访问密码: ")?;
            let rows = store.servers(&password)?;
            if rows.is_empty() {
                println!("暂无已配置的服务器");
            } else {
                println!(
                    "{:<16} {:<16} {:<32} {:<8} {:<12} {:<8} 认证",
                    "ID", "名称", "主机", "端口", "用户名", "方式"
                );
                for r in &rows {
                    println!(
                        "{:<16} {:<16} {:<32} {:<8} {:<12} {:<8} {}",
                        r.id,
                        r.name,
                        r.host,
                        r.port,
                        r.username,
                        r.auth,
                        match &r.key_id {
                            Some(k) => format!("密钥 {}", k),
                            None if r.has_password => "密码".into(),
                            None => "无凭据".into(),
                        }
                    );
                }
            }
        }
        Command::Version => {
            println!("sshweb {}", env!("CARGO_PKG_VERSION"));
        }
    }
    Ok(())
}

/// Prompt for a password. Uses an echo-off prompt on a terminal; falls back to
/// reading a line from stdin (so `echo pass | sshweb keys` works in scripts).
fn read_password(prompt: &str) -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if std::io::stdin().is_terminal() {
        print!("{prompt}");
        std::io::stdout().flush()?;
        Ok(rpassword::read_password()?)
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim().to_string())
    }
}

/// Detach the server into the background: fork, start a new session (no
/// controlling terminal) and redirect stdio so the launching shell prompt
/// returns immediately. In the daemon, stdin is read from `/dev/null` and
/// stdout/stderr are appended to `log` (default `sshweb.log` in the working
/// directory), so background logs stay inspectable. The parent process exits
/// with success once the fork is complete; only the child returns `Ok`.
///
/// Must run **before** the tokio runtime is built and tracing is initialised:
/// the child's stderr redirect must be in place before the logger probes
/// whether stderr is a terminal (otherwise the daemon log would carry ANSI
/// colour codes). Forking single-threaded (pre-runtime) is also the only safe
/// place to fork.
fn daemonize(log: Option<PathBuf>) -> Result<()> {
    use std::os::fd::AsRawFd;

    use nix::unistd::{close, dup2, fork, setsid, ForkResult};

    match unsafe { fork() }.context("fork 失败")? {
        ForkResult::Parent { .. } => {
            // Hand the terminal back to the launching shell.
            std::process::exit(0);
        }
        ForkResult::Child => {
            setsid().context("setsid 失败")?;
            let log_path = log.unwrap_or_else(|| PathBuf::from("sshweb.log"));
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .with_context(|| format!("打开日志文件 {} 失败", log_path.display()))?;
            let devnull = std::fs::File::open("/dev/null").context("打开 /dev/null 失败")?;
            // stdin from /dev/null; stdout + stderr appended to the log file.
            dup2(devnull.as_raw_fd(), 0).context("重定向 stdin 失败")?;
            dup2(log_file.as_raw_fd(), 1).context("重定向 stdout 失败")?;
            dup2(log_file.as_raw_fd(), 2).context("重定向 stderr 失败")?;
            // The originals are no longer needed (fds 1/2 now alias the log).
            let _ = close(devnull.as_raw_fd());
            let _ = close(log_file.as_raw_fd());
            Ok(())
        }
    }
}

/// Initialise the tracing subscriber (stderr; ANSI colours are auto-detected,
/// so a detached daemon's redirected stderr produces plain-text logs).
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or("info".into()))
        .with_writer(std::io::stderr)
        .init();
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Config subcommands (export/import/keys/...) run in the foreground and
    // never start the server, so `-d` does not apply to them.
    if args.command.is_some() {
        init_tracing();
        return match run_config_command(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                error!("{err:?}");
                ExitCode::FAILURE
            }
        };
    }

    // Server run: `-d` detaches into the background before tracing is set up,
    // so the daemon's logs land in the redirect target (plain text, no ANSI).
    if args.detach {
        if let Err(err) = daemonize(args.log.clone()) {
            eprintln!("无法后台启动: {err:#}");
            return ExitCode::FAILURE;
        }
    }

    init_tracing();

    match start(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err:?}");
            ExitCode::FAILURE
        }
    }
}
