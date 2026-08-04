//! GUI 到 sidecar 的常驻终端输入客户端。
//!
//! Business Logic（为什么需要这个模块）:
//!     Tauri invoke 必须在输入进入本机有界队列后立即返回，不能等待 loopback、远端网络或 PTY ACK。
//!
//! Code Logic（这个模块做什么）:
//!     按 control descriptor lazy 建立一条 WebSocket actor；每个 session 使用独立 lane/seq，actor 同时
//!     发送输入与消费 ACK。连接失败会终止 actor，未 ACK 输入不重放，下一次输入创建新 lane。

use crate::backend::ui::BackendUi;
use crate::error::AppError;
use crate::workbench::terminal_input::{
    TerminalInputClientFrame, TerminalInputServerFrame, CONTROL_TOKEN_HEADER,
    TERMINAL_INPUT_SUBPROTOCOL,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

const GUI_INPUT_QUEUE_CAPACITY: usize = 64;

#[derive(Debug)]
struct InputAdmission {
    session_id: String,
    data: String,
}

#[derive(Debug)]
struct ActiveChannel {
    descriptor_key: String,
    sender: mpsc::Sender<InputAdmission>,
}

/// GUI 进程共享的输入 actor runtime。
#[derive(Debug, Default)]
pub struct TerminalInputClientRuntime {
    active: Mutex<Option<ActiveChannel>>,
}

impl TerminalInputClientRuntime {
    /// 将输入接纳到本机有界队列。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     键盘事件的同步路径只允许承担本地 admission 成本；队列满或 actor 已断开必须明确失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     descriptor 未变且 sender 存活时复用；否则创建 actor。使用 try_send，绝不在调用线程等待网络。
    pub fn enqueue(
        &self,
        ui: Arc<dyn BackendUi>,
        descriptor_key: String,
        ws_url: String,
        control_token: String,
        session_id: String,
        data: String,
    ) -> Result<(), AppError> {
        let mut active = self.active.lock().expect("terminal input runtime 锁中毒");
        let must_replace = active.as_ref().map_or(true, |channel| {
            channel.descriptor_key != descriptor_key || channel.sender.is_closed()
        });
        if must_replace {
            let (sender, receiver) = mpsc::channel(GUI_INPUT_QUEUE_CAPACITY);
            tokio::spawn(run_input_actor(ws_url, control_token, receiver, ui));
            *active = Some(ActiveChannel {
                descriptor_key,
                sender,
            });
        }
        active
            .as_ref()
            .expect("刚创建的 terminal input channel 不应缺失")
            .sender
            .try_send(InputAdmission { session_id, data })
            .map_err(|error| {
                AppError::unavailable(format!("terminal_input_queue_rejected: {error}"))
            })
    }
}

async fn run_input_actor(
    ws_url: String,
    control_token: String,
    mut receiver: mpsc::Receiver<InputAdmission>,
    ui: Arc<dyn BackendUi>,
) {
    let known_sessions = Arc::new(Mutex::new(HashSet::<String>::new()));
    if let Err(error) = run_connected_input_actor(
        &ws_url,
        &control_token,
        &mut receiver,
        known_sessions.clone(),
        ui.clone(),
    )
    .await
    {
        while let Ok(admission) = receiver.try_recv() {
            known_sessions
                .lock()
                .expect("terminal input sessions 锁中毒")
                .insert(admission.session_id);
        }
        let sessions = known_sessions
            .lock()
            .expect("terminal input sessions 锁中毒")
            .clone();
        for session_id in sessions {
            ui.emit(
                "workbench:terminal-input-state",
                serde_json::json!({
                    "sessionId": session_id,
                    "status": "blocked",
                    "code": "connectionLostUnknown",
                    "message": "终端输入连接已中断；未确认输入不会自动重放"
                }),
            );
        }
        tracing::warn!(error = %error, "桌面终端输入流已中断；未确认输入不会自动重放");
    }
}

async fn run_connected_input_actor(
    ws_url: &str,
    control_token: &str,
    receiver: &mut mpsc::Receiver<InputAdmission>,
    known_sessions: Arc<Mutex<HashSet<String>>>,
    ui: Arc<dyn BackendUi>,
) -> Result<(), AppError> {
    let mut request = ws_url
        .into_client_request()
        .map_err(|error| AppError::validation(format!("control terminal WS URL 无效: {error}")))?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(TERMINAL_INPUT_SUBPROTOCOL),
    );
    request.headers_mut().insert(
        CONTROL_TOKEN_HEADER,
        HeaderValue::from_str(control_token)
            .map_err(|_| AppError::validation("control token 不能写入请求头"))?,
    );
    let (socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|error| {
            AppError::unavailable(format!("连接 control terminal input 失败: {error}"))
        })?;
    if response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        != Some(TERMINAL_INPUT_SUBPROTOCOL)
    {
        return Err(AppError::conflict("control terminal input 子协议不匹配"));
    }
    let (mut sink, mut stream) = socket.split();
    let hello = TerminalInputClientFrame::Hello {
        client_id: format!("gui-{}", uuid::Uuid::new_v4()),
    };
    sink.send(Message::Text(serde_json::to_string(&hello)?))
        .await
        .map_err(|error| {
            AppError::unavailable(format!("发送 control terminal hello 失败: {error}"))
        })?;
    let ready = stream
        .next()
        .await
        .ok_or_else(|| AppError::unavailable("control terminal input 未返回 ready"))?
        .map_err(|error| {
            AppError::unavailable(format!("读取 control terminal ready 失败: {error}"))
        })?;
    let Message::Text(ready_text) = ready else {
        return Err(AppError::conflict("control terminal ready 帧无效"));
    };
    if !matches!(
        serde_json::from_str::<TerminalInputServerFrame>(&ready_text)?,
        TerminalInputServerFrame::Ready { .. }
    ) {
        return Err(AppError::conflict("control terminal 首个响应不是 ready"));
    }

    let mut lanes: HashMap<String, (String, u64)> = HashMap::new();
    loop {
        tokio::select! {
            admission = receiver.recv() => {
                let Some(admission) = admission else { return Ok(()); };
                known_sessions
                    .lock()
                    .expect("terminal input sessions 锁中毒")
                    .insert(admission.session_id.clone());
                let lane = lanes.entry(admission.session_id.clone())
                    .or_insert_with(|| (uuid::Uuid::new_v4().to_string(), 0));
                lane.1 += 1;
                let frame = TerminalInputClientFrame::Input {
                    lane_id: lane.0.clone(), session_id: admission.session_id, seq: lane.1, data: admission.data,
                };
                sink.send(Message::Text(serde_json::to_string(&frame)?)).await
                    .map_err(|error| AppError::unavailable(format!("control terminal input 发送失败: {error}")))?;
            }
            response = stream.next() => {
                let Some(response) = response else {
                    return Err(AppError::unavailable("control terminal input 连接关闭"));
                };
                let response = response.map_err(|error| AppError::unavailable(format!("control terminal input 接收失败: {error}")))?;
                if let Message::Text(text) = response {
                    if let Ok(TerminalInputServerFrame::Error { session_id: Some(session_id), code, message, .. }) = serde_json::from_str(&text) {
                        lanes.remove(&session_id);
                        ui.emit(
                            "workbench:terminal-input-state",
                            serde_json::json!({
                                "sessionId": session_id,
                                "status": "blocked",
                                "code": code,
                                "message": message
                            }),
                        );
                    }
                }
            }
        }
    }
}
