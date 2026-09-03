use std::sync::Arc;

use anyhow::Result;
use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path, State,
};
use axum::http::HeaderMap;
use axum::response::Response;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{error, info_span, warn, Instrument};

use crate::session::Session;
use crate::web::protocol::{WsClient, WsServer};
use crate::ServerState;

/// Cap on a single inbound WebSocket message (安全审查 M4). Chunked uploads
/// send 240 KiB slices; editor saves can be a few MB. 16 MiB bounds a
/// malicious client from pushing multi-hundred-MB frames into the write queue.
const MAX_WS_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

pub async fn get_session_ws(
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    // The browser sends the same-origin HttpOnly cookie during the WebSocket
    // upgrade. Reject unauthenticated upgrades before creating a session.
    if let Some(resp) = super::require_auth(&state, &headers) {
        return resp;
    }
    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            let span = info_span!("ws");
            async move {
                // The path segment is the stable session key: reuse the live
                // session when one exists (a browser refresh reconnects to the
                // same terminal processes), otherwise create it and spawn the
                // initial shell.
                let session = state.get_or_create(&name, || {
                    let session = Session::new(name.clone(), state.shell_command(), state.config());
                    if let Err(err) = session.create_shell(0, 0, None, None, String::new()) {
                        warn!(?err, "failed to spawn initial shell");
                    }
                    session
                });

                let (sub_id, mut output_rx) = session.attach();

                if let Err(err) = handle_socket(socket, &session, &mut output_rx).await {
                    warn!(?err, "websocket exiting early");
                }

                // Disconnect must NOT destroy the session: its terminal processes
                // keep running (with output buffered) so a refresh can reattach.
                // Idle sessions are reclaimed later by ServerState::reclaim_idle.
                session.detach(sub_id);
            }
            .instrument(span)
        })
}

/// Serialize a server message into a binary WebSocket message.
fn encode(msg: WsServer) -> Result<Message> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&msg, &mut buf)?;
    Ok(Message::Binary(Bytes::from(buf)))
}

/// Receive a message from the client over WebSocket.
async fn recv(
    stream: &mut (impl StreamExt<Item = Result<Message, axum::Error>> + Unpin),
) -> Result<Option<WsClient>> {
    Ok(loop {
        match stream.next().await.transpose()? {
            Some(Message::Text(_)) => warn!("ignoring text message over WebSocket"),
            Some(Message::Binary(msg)) => break Some(ciborium::de::from_reader(&*msg)?),
            Some(_) => (), // ignore other message types, keep looping
            None => break None,
        }
    })
}

/// Handle an incoming live WebSocket connection to a given session.
///
/// The send side is driven by a dedicated writer task fed through a channel,
/// so a slow / large outgoing message (e.g. a huge SFTP listing or a burst of
/// terminal chunks) never blocks the receive loop. Otherwise the whole session
/// freezes: one blocked `socket.send` prevents the loop from reading the
/// client's next message, and every operation appears stuck.
async fn handle_socket(
    socket: WebSocket,
    session: &Arc<Session>,
    output_rx: &mut mpsc::Receiver<WsServer>,
) -> Result<()> {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<WsServer>(512);

    // Writer task: drains the outbound queue into the socket. It does not
    // await on anything else, so backpressure from a slow client only blocks
    // this task, never the message-processing loop.
    let writer = tokio::spawn(async move {
        let mut result: Result<()> = Ok(());
        while let Some(msg) = out_rx.recv().await {
            match encode(msg) {
                Ok(m) => {
                    if let Err(err) = sink.send(m).await {
                        result = Err(err.into());
                        break;
                    }
                }
                Err(err) => {
                    result = Err(err);
                    break;
                }
            }
        }
        result
    });

    let session_owned = Arc::clone(session);

    let loop_result: Result<()> = async {
        loop {
            tokio::select! {
                _ = session_owned.terminated() => break Ok(()),
                Some(msg) = output_rx.recv() => {
                    let _ = out_tx.try_send(msg);
                }
                result = recv(&mut stream) => {
                    match result? {
                        Some(msg) => dispatch(&session_owned, msg),
                        None => break Ok(()),
                    }
                }
            }
        }
    }
    .await;

    // Signal the writer to stop and wait for it to flush/close.
    drop(out_tx);
    writer.await??;
    loop_result?;
    Ok(())
}

/// Clone the session into a background task created by `create`, reporting any
/// error through the session's bounded output queue. All slow SFTP operations
/// run this way so the message loop never awaits on network/file I/O.
fn spawn_op<F, Fut>(session: &Arc<Session>, label: &'static str, create: F)
where
    F: FnOnce(Arc<Session>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let session = Arc::clone(session);
    tokio::spawn(async move {
        if let Err(e) = create(Arc::clone(&session)).await {
            session.error(format!("{label}：{err:#}", err = e));
        }
    });
}

/// Dispatch a single client message. All slow operations are spawned onto
/// separate tasks; nothing here awaits on network/file I/O. Errors use the
/// same bounded output queue as other server messages.
fn dispatch(session: &Arc<Session>, msg: WsClient) {
    match msg {
        WsClient::Create(x, y, server, cwd, label) => {
            if let Err(e) = session.create_shell(x, y, server, cwd, label.unwrap_or_default()) {
                session.error(e.to_string());
            }
        }
        WsClient::SftpConnect(mut server) => {
            // Resolve key-mode auth before storing the headless shell, so the
            // pool shares a config that already carries the private key.
            if let Err(e) = session.resolve_auth(&mut server) {
                session.error(e.to_string());
                return;
            }
            let sid = session.connect_sftp_shell(server);
            // Probe the initial directory and effective user off the socket
            // loop. The probe runs on the same SSH connection the SFTP pool
            // will reuse, so the first listing needs no second handshake.
            spawn_op(session, "SFTP 探测", move |s| async move {
                s.sftp_probe(sid).await;
                Ok(())
            });
        }
        WsClient::SftpOpen(id) => {
            spawn_op(session, "SFTP 打开", move |s| async move {
                s.sftp_open(id).await;
                Ok(())
            });
        }
        WsClient::SetActive(id) => {
            session.set_active(id);
        }
        WsClient::ReorderShells(ids) => {
            if let Err(e) = session.reorder_shells(ids) {
                session.error(e.to_string());
            }
        }
        WsClient::Close(id) => {
            if let Err(e) = session.close_shell(id) {
                session.error(e.to_string());
            }
        }
        WsClient::Resize(id, winsize) => {
            if let Err(err) = session.resize_shell(id, winsize) {
                error!(%id, ?err, "failed to resize shell");
                session.error(err.to_string());
            }
        }
        WsClient::Data(id, data) => {
            session.send_input(id, data.to_vec());
        }
        WsClient::PwdRequest(id) => {
            session.pwd_request(id);
        }
        WsClient::SftpList(id, path) => {
            spawn_op(session, "列表失败", move |s| async move {
                s.sftp_list(id, path).await?;
                Ok(())
            });
        }
        WsClient::SftpRead(id, path) => {
            spawn_op(session, "读取失败", move |s| async move {
                s.sftp_read(id, path).await?;
                Ok(())
            });
        }
        WsClient::SftpWriteAt(id, path, offset, data) => {
            // Ordered queue: applies chunks sequentially, never blocks the loop.
            session.enqueue_write_at(id, path, offset, data);
        }
        WsClient::SftpWrite(id, path, data) => {
            // Whole-file saves share the FIFO write queue with chunked
            // uploads, so save-vs-upload ordering is preserved.
            session.enqueue_write(id, path, data);
        }
        WsClient::SftpMkdir(id, path) => {
            spawn_op(session, "创建目录失败", move |s| async move {
                s.sftp_mkdir(id, path).await?;
                Ok(())
            });
        }
        WsClient::SftpRemove(id, path, is_dir) => {
            spawn_op(session, "删除失败", move |s| async move {
                s.sftp_remove(id, path, is_dir).await?;
                Ok(())
            });
        }
        WsClient::SftpRename(id, from, to) => {
            spawn_op(session, "重命名失败", move |s| async move {
                s.sftp_rename(id, from, to).await?;
                Ok(())
            });
        }
        WsClient::SftpCopy(id, from, to) => {
            spawn_op(session, "复制失败", move |s| async move {
                s.sftp_copy(id, from, to).await?;
                Ok(())
            });
        }
    }
}
