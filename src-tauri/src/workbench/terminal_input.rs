//! Workbench 终端输入 WebSocket 协议与 owner 网关。
//!
//! Business Logic（为什么需要这个模块）:
//!     交互式终端输入不能为每个按键执行 health + HTTP mutation 往返；桌面、mobile 与远端
//!     owning device 需要复用常驻连接，并在 PTY flush 后返回 ACK。
//!
//! Code Logic（这个模块做什么）:
//!     定义 v1 文本帧，处理 local-only / remote-aware WebSocket；远端连接只在建链时做一次
//!     capability 与 device 绑定检查，之后直接转发输入帧，绝不自动重放断线时未确认的输入。

use crate::commands::workbench::{
    device_base_url, local_write_workbench_session_input, remote_inner_session_id,
};
use crate::error::AppError;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::protocol::CAPABILITY_WORKBENCH_TERMINAL_INPUT_STREAM_V1;
use crate::state::AppState;
use crate::workbench::remote_ids::parse_remote_entity_id;
use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::HeaderValue, Message as TungsteniteMessage,
};

/// 终端输入 WS 子协议。
pub const TERMINAL_INPUT_SUBPROTOCOL: &str = "cc-partner.terminal-input.v1";
/// control WS 令牌 header；令牌不得出现在 URL 或日志中。
pub const CONTROL_TOKEN_HEADER: &str = "x-cc-partner-control-token";
/// 单个输入帧正文上限。
pub const MAX_INPUT_FRAME_BYTES: usize = 32 * 1024;
const OUTBOUND_QUEUE_CAPACITY: usize = 64;

/// 客户端到 owner 的 v1 帧。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TerminalInputClientFrame {
    Hello {
        client_id: String,
    },
    Input {
        lane_id: String,
        session_id: String,
        seq: u64,
        data: String,
    },
    Ping {
        nonce: String,
    },
}

/// owner 返回客户端的 v1 帧。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TerminalInputServerFrame {
    Ready {
        device_id: String,
    },
    Ack {
        lane_id: String,
        session_id: String,
        seq: u64,
    },
    Error {
        lane_id: Option<String>,
        session_id: Option<String>,
        seq: Option<u64>,
        code: String,
        message: String,
    },
    Pong {
        nonce: String,
    },
}

/// 输入网关是否允许解析 remote composite session ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalInputGatewayMode {
    LocalOnly,
    RemoteAware,
}

type OutboundSender = mpsc::Sender<TerminalInputServerFrame>;
#[derive(Debug)]
struct PeerForward {
    frame: TerminalInputClientFrame,
    outer_session_id: String,
}

type PeerInputSender = mpsc::Sender<PeerForward>;

/// 处理一条终端输入 WebSocket。
///
/// Business Logic（为什么需要这个函数）:
///     三个表面共享完全相同的帧/ACK 语义，差别只在是否允许 remote composite session。
///
/// Code Logic（这个函数做什么）:
///     拆分 socket，以有界队列串行写响应；校验 hello/seq/大小，本地直接写 PTY，远端按设备
///     lazy 建立一条 peer WS。ACK 不作为后续 input 的发送闸门。
pub async fn serve_terminal_input_socket(
    socket: WebSocket,
    state: AppState,
    mode: TerminalInputGatewayMode,
) {
    let (mut ws_sink, mut ws_stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<TerminalInputServerFrame>(OUTBOUND_QUEUE_CAPACITY);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let Ok(text) = serde_json::to_string(&frame) else {
                break;
            };
            if ws_sink.send(AxumMessage::Text(text)).await.is_err() {
                break;
            }
        }
    });

    let mut hello_seen = false;
    let mut lanes: HashMap<String, u64> = HashMap::new();
    let mut blocked_lanes: HashSet<String> = HashSet::new();
    let mut peer_links: HashMap<String, PeerInputSender> = HashMap::new();

    while let Some(message) = ws_stream.next().await {
        let Ok(AxumMessage::Text(text)) = message else {
            continue;
        };
        let frame = match serde_json::from_str::<TerminalInputClientFrame>(&text) {
            Ok(frame) => frame,
            Err(_) => {
                send_error(
                    &out_tx,
                    None,
                    None,
                    None,
                    "malformedFrame",
                    "输入帧格式无效",
                )
                .await;
                continue;
            }
        };
        match frame {
            TerminalInputClientFrame::Hello { client_id } => {
                if hello_seen || client_id.trim().is_empty() {
                    send_error(&out_tx, None, None, None, "invalidHello", "hello 帧无效").await;
                    break;
                }
                hello_seen = true;
                let device_id = state
                    .config
                    .read()
                    .expect("config 读锁中毒")
                    .device_id
                    .clone();
                let _ = out_tx
                    .send(TerminalInputServerFrame::Ready { device_id })
                    .await;
            }
            TerminalInputClientFrame::Ping { nonce } if hello_seen => {
                let _ = out_tx.send(TerminalInputServerFrame::Pong { nonce }).await;
            }
            TerminalInputClientFrame::Input {
                lane_id,
                session_id,
                seq,
                data,
            } if hello_seen => {
                if lane_id.trim().is_empty() || session_id.trim().is_empty() || seq == 0 {
                    send_error(
                        &out_tx,
                        Some(lane_id),
                        Some(session_id),
                        Some(seq),
                        "invalidInput",
                        "输入帧字段无效",
                    )
                    .await;
                    continue;
                }
                if data.len() > MAX_INPUT_FRAME_BYTES {
                    blocked_lanes.insert(lane_id.clone());
                    send_error(
                        &out_tx,
                        Some(lane_id),
                        Some(session_id),
                        Some(seq),
                        "frameTooLarge",
                        "输入帧超过 32 KiB",
                    )
                    .await;
                    continue;
                }
                if blocked_lanes.contains(&lane_id) {
                    send_error(
                        &out_tx,
                        Some(lane_id),
                        Some(session_id),
                        Some(seq),
                        "laneBlocked",
                        "输入通道已封锁，请重新建立通道",
                    )
                    .await;
                    continue;
                }
                let expected = lanes.get(&lane_id).copied().unwrap_or(0) + 1;
                if seq != expected {
                    blocked_lanes.insert(lane_id.clone());
                    send_error(
                        &out_tx,
                        Some(lane_id),
                        Some(session_id),
                        Some(seq),
                        "sequenceGap",
                        "输入序号不连续，通道已封锁",
                    )
                    .await;
                    continue;
                }
                lanes.insert(lane_id.clone(), seq);

                if let Some(parsed) = parse_remote_entity_id(&session_id) {
                    if mode == TerminalInputGatewayMode::LocalOnly {
                        blocked_lanes.insert(lane_id.clone());
                        send_error(
                            &out_tx,
                            Some(lane_id),
                            Some(session_id),
                            Some(seq),
                            "proxyLoopRejected",
                            "peer 输入端只接受本机会话",
                        )
                        .await;
                        continue;
                    }
                    let inner = match remote_inner_session_id(&parsed.device_id, &session_id) {
                        Ok(value) => value,
                        Err(error) => {
                            blocked_lanes.insert(lane_id.clone());
                            send_app_error(&out_tx, lane_id, session_id, seq, error).await;
                            continue;
                        }
                    };
                    let link = match peer_link_for_device(
                        &state,
                        &parsed.device_id,
                        &out_tx,
                        &mut peer_links,
                    )
                    .await
                    {
                        Ok(link) => link,
                        Err(error) => {
                            blocked_lanes.insert(lane_id.clone());
                            send_app_error(&out_tx, lane_id, session_id, seq, error).await;
                            continue;
                        }
                    };
                    let remote_frame = TerminalInputClientFrame::Input {
                        lane_id: lane_id.clone(),
                        session_id: inner,
                        seq,
                        data,
                    };
                    if link
                        .send(PeerForward {
                            frame: remote_frame,
                            outer_session_id: session_id.clone(),
                        })
                        .await
                        .is_err()
                    {
                        peer_links.remove(&parsed.device_id);
                        blocked_lanes.insert(lane_id.clone());
                        send_error(
                            &out_tx,
                            Some(lane_id),
                            Some(session_id),
                            Some(seq),
                            "connectionLostUnknown",
                            "远端输入连接已断开，未确认输入不会自动重放",
                        )
                        .await;
                    }
                } else {
                    match write_local_input(&state, &session_id, &data).await {
                        Ok(()) => {
                            let _ = out_tx
                                .send(TerminalInputServerFrame::Ack {
                                    lane_id,
                                    session_id,
                                    seq,
                                })
                                .await;
                        }
                        Err(error) => {
                            blocked_lanes.insert(lane_id.clone());
                            send_app_error(&out_tx, lane_id, session_id, seq, error).await;
                        }
                    }
                }
            }
            _ => {
                send_error(
                    &out_tx,
                    None,
                    None,
                    None,
                    "helloRequired",
                    "首帧必须是 hello",
                )
                .await;
                break;
            }
        }
    }
    drop(out_tx);
    for (_, link) in peer_links.drain() {
        drop(link);
    }
    writer.abort();
}

async fn write_local_input(state: &AppState, session_id: &str, data: &str) -> Result<(), AppError> {
    let row = state
        .workbench_session_repo
        .get(session_id)
        .await?
        .ok_or_else(|| AppError::not_found("Workbench 会话不存在"))?;
    let project = state
        .workbench_project_repo
        .get(&row.project_id)
        .await?
        .ok_or_else(|| AppError::not_found("Workbench 项目不存在"))?;
    if project.kind != "local" {
        return Err(AppError::validation("输入网关只接受本机项目会话"));
    }
    local_write_workbench_session_input(state, session_id.to_string(), data.to_string()).await?;
    Ok(())
}

async fn peer_link_for_device(
    state: &AppState,
    device_id: &str,
    outbound: &OutboundSender,
    links: &mut HashMap<String, PeerInputSender>,
) -> Result<PeerInputSender, AppError> {
    if let Some(sender) = links.get(device_id).filter(|sender| !sender.is_closed()) {
        return Ok(sender.clone());
    }
    let base_url = device_base_url(state, device_id)?;
    let health = state
        .peer_client
        .require_capability(&base_url, CAPABILITY_WORKBENCH_TERMINAL_INPUT_STREAM_V1)
        .await
        .map_err(|error| AppError::unavailable(format!("远端终端输入能力不可用: {error}")))?;
    if health.device_id != device_id {
        return Err(AppError::conflict("远端终端输入设备标识不匹配"));
    }
    let sender = connect_peer_link(base_url, device_id.to_string(), outbound.clone()).await?;
    links.insert(device_id.to_string(), sender.clone());
    Ok(sender)
}

async fn connect_peer_link(
    base_url: String,
    device_id: String,
    outbound: OutboundSender,
) -> Result<PeerInputSender, AppError> {
    let ws_url = format!(
        "{}/api/workbench/terminal-input-stream",
        base_url.replacen("http", "ws", 1)
    );
    let mut request = ws_url
        .into_client_request()
        .map_err(|error| AppError::validation(format!("终端输入 WS URL 无效: {error}")))?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TERMINAL_INPUT_SUBPROTOCOL),
    );
    request.headers_mut().insert(
        EXPECTED_DEVICE_ID_HEADER.clone(),
        HeaderValue::from_str(&device_id)
            .map_err(|_| AppError::validation("远端设备 ID 不能写入请求头"))?,
    );
    let (socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|error| AppError::unavailable(format!("连接远端终端输入流失败: {error}")))?;
    if response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        != Some(TERMINAL_INPUT_SUBPROTOCOL)
    {
        return Err(AppError::conflict("远端终端输入子协议不匹配"));
    }
    let (mut sink, mut stream) = socket.split();
    let hello = serde_json::to_string(&TerminalInputClientFrame::Hello {
        client_id: format!("peer-{device_id}"),
    })?;
    sink.send(TungsteniteMessage::Text(hello))
        .await
        .map_err(|error| AppError::unavailable(format!("发送终端输入 hello 失败: {error}")))?;
    let ready = stream
        .next()
        .await
        .ok_or_else(|| AppError::unavailable("远端终端输入流未返回 ready"))?
        .map_err(|error| AppError::unavailable(format!("读取终端输入 ready 失败: {error}")))?;
    let TungsteniteMessage::Text(ready_text) = ready else {
        return Err(AppError::conflict("远端终端输入 ready 帧无效"));
    };
    match serde_json::from_str::<TerminalInputServerFrame>(&ready_text)? {
        TerminalInputServerFrame::Ready { device_id: actual } if actual == device_id => {}
        _ => return Err(AppError::conflict("远端终端输入 ready 设备标识不匹配")),
    }
    let (tx, mut rx) = mpsc::channel::<PeerForward>(OUTBOUND_QUEUE_CAPACITY);
    tokio::spawn(async move {
        let mut outer_sessions: HashMap<(String, u64), String> = HashMap::new();
        loop {
            tokio::select! {
                forward = rx.recv() => {
                    let Some(forward) = forward else { break; };
                    if let TerminalInputClientFrame::Input { lane_id, seq, .. } = &forward.frame {
                        outer_sessions.insert((lane_id.clone(), *seq), forward.outer_session_id);
                    }
                    let Ok(text) = serde_json::to_string(&forward.frame) else { break; };
                    if sink.send(TungsteniteMessage::Text(text)).await.is_err() { break; }
                }
                message = stream.next() => {
                    let Some(Ok(TungsteniteMessage::Text(text))) = message else { break; };
                    if let Ok(mut frame) = serde_json::from_str::<TerminalInputServerFrame>(&text) {
                        match &mut frame {
                            TerminalInputServerFrame::Ack { lane_id, session_id, seq } => {
                                if let Some(outer) = outer_sessions.remove(&(lane_id.clone(), *seq)) {
                                    *session_id = outer;
                                }
                            }
                            TerminalInputServerFrame::Error { lane_id: Some(lane_id), session_id: Some(session_id), seq: Some(seq), .. } => {
                                if let Some(outer) = outer_sessions.remove(&(lane_id.clone(), *seq)) {
                                    *session_id = outer;
                                }
                            }
                            _ => {}
                        }
                        if outbound.send(frame).await.is_err() { break; }
                    }
                }
            }
        }
    });
    Ok(tx)
}

async fn send_app_error(
    outbound: &OutboundSender,
    lane_id: String,
    session_id: String,
    seq: u64,
    error: AppError,
) {
    send_error(
        outbound,
        Some(lane_id),
        Some(session_id),
        Some(seq),
        "writeFailed",
        &error.to_string(),
    )
    .await;
}

async fn send_error(
    outbound: &OutboundSender,
    lane_id: Option<String>,
    session_id: Option<String>,
    seq: Option<u64>,
    code: &str,
    message: &str,
) {
    let _ = outbound
        .send(TerminalInputServerFrame::Error {
            lane_id,
            session_id,
            seq,
            code: code.to_string(),
            message: message.to_string(),
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_frames_use_stable_camel_case_shape() {
        let frame = TerminalInputClientFrame::Input {
            lane_id: "lane-1".into(),
            session_id: "session-1".into(),
            seq: 2,
            data: "你".into(),
        };
        let json = serde_json::to_value(frame).unwrap();
        assert_eq!(json["type"], "input");
        assert_eq!(json["laneId"], "lane-1");
        assert_eq!(json["sessionId"], "session-1");
        assert_eq!(json["seq"], 2);
        assert_eq!(json["data"], "你");
    }
}
