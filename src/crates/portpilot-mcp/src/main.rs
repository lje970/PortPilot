// PortPilot MCP server(stdio JSON-RPC 2.0)
// ==========================================
// 独立小进程:复用 portpilot-core,让 AI Agent(Claude/Cursor 等支持 MCP 的宿主)
// 直接枚举串口、开关会话、收发数据 —— "AI ready" 差异化卖点的落地形态。
//
// 注册到 AI 宿主(示例):
//   claude mcp add portpilot -- D:\serialport\src\target\release\portpilot-mcp.exe
//
// 工具清单:
//   list_ports / open_session {port,baud} / close_session {id}
//   send {id,data,hex?} / read_available {id} / wait_for {id,pattern,timeout_ms}
//   stats {id}
//
// 实现说明:stdio 每行一条 JSON-RPC;会话层直接用 portpilot-core 的
// spawn_session + 命名抽头(读通道,与 UI 泵同源);本进程无 UI 线程,抽头即唯一消费者。

use portpilot_core as core;
use portpilot_core::SessionHandle;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

struct ManagedSession {
    handle: SessionHandle,
    acc: Arc<Mutex<Vec<u8>>>, // 抽头累加缓冲(read_available/wait_from 的数据源)
    encoding: Arc<std::sync::atomic::AtomicU8>,
    port: String,
}

struct McpState {
    sessions: Mutex<HashMap<String, ManagedSession>>,
    next_id: AtomicU64,
}

fn decode(raw: &[u8], enc: u8) -> String {
    let (cow, _, _) = if enc == 1 {
        encoding_rs::GBK.decode(raw)
    } else {
        encoding_rs::UTF_8.decode(raw)
    };
    cow.into_owned()
}

const TOOLS: &str = r#"[
  {"name":"list_ports","description":"枚举系统串口(含 USB VID/PID)","inputSchema":{"type":"object","properties":{}}},
  {"name":"open_session","description":"打开串口会话","inputSchema":{"type":"object","properties":{"port":{"type":"string"},"baud":{"type":"integer","default":115200}},"required":["port"]}},
  {"name":"close_session","description":"关闭会话","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
  {"name":"send","description":"发送文本或 HEX 数据","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"data":{"type":"string"},"hex":{"type":"boolean","default":false},"newline":{"type":"string","enum":["none","cr","lf","crlf"],"default":"crlf"}},"required":["id","data"]}},
  {"name":"read_available","description":"读取当前已到达的数据(文本,清空缓冲)","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
  {"name":"wait_for","description":"等待出现指定关键词,返回累计文本","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"pattern":{"type":"string"},"timeout_ms":{"type":"integer","default":3000}},"required":["id","pattern"]}},
  {"name":"stats","description":"会话统计(RX/TX/连接状态)","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}}
]"#;

fn tools_list() -> Value {
    json!({ "tools": serde_json::from_str::<Value>(TOOLS).unwrap() })
}

impl McpState {
    fn dispatch(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method")?.as_str()?.to_string();
        let id = msg.get("id").cloned();
        if id.is_none() {
            return None; // 通知:不回包
        }
        let result: Result<Value, String> = match method.as_str() {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "portpilot-mcp", "version": "0.1.0" }
            })),
            "tools/list" => Ok(tools_list()),
            "tools/call" => self.tools_call(msg),
            "ping" => Ok(json!({})),
            other => Err(format!("未知方法: {other}")),
        };
        Some(match result {
            Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id,
                              "error": { "code": -32000, "message": e } }),
        })
    }

    fn tools_call(&mut self, msg: &Value) -> Result<Value, String> {
        let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
        let args = &msg["params"]["arguments"];
        let text = match name.as_str() {
            "list_ports" => {
                let list: Vec<String> = core::list_ports()
                    .into_iter()
                    .map(|p| match (p.usb_vid, p.usb_pid) {
                        (Some(v), Some(d)) => format!("{} (USB {:04X}:{:04X})", p.name, v, d),
                        _ => p.name,
                    })
                    .collect();
                list.join("\n")
            }
            "open_session" => {
                let port = args["port"].as_str().unwrap_or("").to_string();
                let baud = args["baud"].as_u64().unwrap_or(115200) as u32;
                let kind = args["kind"].as_str().unwrap_or("serial").to_string();
                let id = format!("{kind}:{port}");
                if self.sessions.lock().unwrap().contains_key(&id) {
                    return Ok(json!({ "content": [{ "type": "text", "text": format!("会话 {id} 已存在") }] }));
                }
                let acc: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
                let encoding = Arc::new(std::sync::atomic::AtomicU8::new(0));
                let acc2 = Arc::clone(&acc);
                let cfg = core::TransportConfig {
                    kind: core::TransportKind::from_str(&kind),
                    port: port.clone(),
                    remote: None,
                    baud,
                    data_bits: 8,
                    stop_bits: 1,
                    parity: core::Parity::None,
                    read_timeout_ms: 5,
                };
                let handle = core::spawn_session(core::SessionOptions::new(Box::new(move || {
                    core::open_transport(&cfg)
                })));
                handle.add_tap(
                    "mcp",
                    Box::new(move |d: &[u8]| acc2.lock().unwrap().extend_from_slice(d)),
                );
                self.sessions.lock().unwrap().insert(
                    id.clone(),
                    ManagedSession { handle, acc, encoding, port: id.clone() },
                );
                format!("会话 {id} 已打开 @ {baud}")
            }
            "close_session" => {
                let id = args["id"].as_str().unwrap_or("").to_string();
                match self.sessions.lock().unwrap().remove(&id) {
                    Some(e) => {
                        e.handle.stop();
                        format!("会话 {id} 已关闭")
                    }
                    None => format!("会话 {id} 不存在"),
                }
            }
            "send" => {
                let id = args["id"].as_str().unwrap_or("").to_string();
                let data = args["data"].as_str().unwrap_or("").to_string();
                let hex = args["hex"].as_bool().unwrap_or(false);
                let nl = args["newline"].as_str().unwrap_or("crlf").to_string();
                let mut sessions = self.sessions.lock().unwrap();
                let e = sessions.get_mut(&id).ok_or("会话不存在")?;
                let bytes: Vec<u8> = if hex {
                    let cleaned: String = data.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                    (0..cleaned.len())
                        .step_by(2)
                        .filter_map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
                        .collect()
                } else {
                    let mut s = data;
                    match nl.as_str() {
                        "cr" => s.push('\r'),
                        "lf" => s.push('\n'),
                        "crlf" => s.push_str("\r\n"),
                        _ => {}
                    }
                    let enc = e.encoding.load(Ordering::Relaxed);
                    encode_text(&s, enc)
                };
                let n = e.handle.write(&bytes).map_err(|e| e.to_string())?;
                format!("已发送 {n}B")
            }
            "read_available" => {
                let id = args["id"].as_str().unwrap_or("").to_string();
                let mut sessions = self.sessions.lock().unwrap();
                let e = sessions.get_mut(&id).ok_or("会话不存在")?;
                let enc = e.encoding.load(Ordering::Relaxed);
                let mut a = e.acc.lock().unwrap();
                let text = decode(&a, enc);
                a.clear();
                text
            }
            "wait_for" => {
                let id = args["id"].as_str().unwrap_or("").to_string();
                let pattern = args["pattern"].as_str().unwrap_or("").to_string();
                let timeout = args["timeout_ms"].as_u64().unwrap_or(3000);
                let mut sessions = self.sessions.lock().unwrap();
                let e = sessions.get_mut(&id).ok_or("会话不存在")?;
                let enc = e.encoding.load(Ordering::Relaxed);
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_millis(timeout);
                let pat = pattern.into_bytes();
                loop {
                    {
                        let mut a = e.acc.lock().unwrap();
                        if let Some(pos) = find_sub(&a, &pat) {
                            let text = decode(&a, enc);
                            a.drain(..pos + pat.len());
                            return Ok(json!({
                                "content": [{ "type": "text",
                                    "text": format!("[命中] {text}") }],
                            }));
                        }
                    }
                    if std::time::Instant::now() >= deadline {
                        let a = e.acc.lock().unwrap();
                        return Ok(json!({
                            "content": [{ "type": "text",
                                "text": format!("[未命中/超时] {}", decode(&a, enc)) }],
                        }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            "stats" => {
                let id = args["id"].as_str().unwrap_or("").to_string();
                let mut sessions = self.sessions.lock().unwrap();
                let e = sessions.get_mut(&id).ok_or("会话不存在")?;
                let st = e.handle.stats();
                format!("connected={} rx={} tx={} reconnects={}",
                    st.connected, st.rx_bytes, st.tx_bytes, st.reconnects)
            }
            other => return Err(format!("未知工具: {other}")),
        };
        Ok(json!({ "content": [{ "type": "text", "text": text }] }))
    }
}

fn encode_text(s: &str, _enc: u8) -> Vec<u8> {
    s.as_bytes().to_vec() // MVP:发送固定 UTF-8;GBK 发送待 P2 后续
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn dispatch(state: &mut McpState, msg: &Value) -> Option<Value> {
    state.dispatch(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_initialize_and_tools_list() {
        let mut st = McpState {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        };
        let r = st.dispatch(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {} })).unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "portpilot-mcp");
        let r = st.dispatch(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list",
            "params": {} })).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 7, "工具数量 {}", tools.len());
    }

    #[test]
    fn mcp_loop_roundtrip() {
        let mut st = McpState {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        };
        let r = st.dispatch(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "open_session",
                "arguments": { "port": "", "kind": "loop", "baud": 0 } } }))
            .unwrap();
        assert!(r["result"]["content"][0]["text"]
            .as_str().unwrap().contains("已打开"), "打开失败: {r}");
        st.dispatch(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let r = st.dispatch(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "send",
                "arguments": { "id": "loop:", "data": "PING", "newline": "none" } } }))
            .unwrap();
        assert!(!r.get("error").is_some(), "send 失败: {r}");
        let r = st.dispatch(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "wait_for",
                "arguments": { "id": "loop:", "pattern": "PING", "timeout_ms": 3000 } } }))
            .unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[命中]"), "未命中: {text}");
        assert!(text.contains("PING"));
    }
}

fn main() {
    let mut state = McpState {
        sessions: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    eprintln!("[portpilot-mcp] 已启动(stdio JSON-RPC)");
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(
                    out,
                    "{}",
                    json!({ "jsonrpc": "2.0", "id": null,
                            "error": { "code": -32700, "message": format!("解析错误: {e}") } })
                );
                continue;
            }
        };
        if let Some(resp) = dispatch(&mut state, &msg) {
            let _ = writeln!(out, "{resp}");
            out.flush().ok();
        }
    }
}
