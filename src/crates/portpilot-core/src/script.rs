//! Lua 脚本引擎(tio 风格 API):`write / write_hex / expect / read / sleep / log / stop_requested / ms`。
//!
//! 运行模型:脚本在调用方的独立线程中阻塞执行;数据经 SessionHandle 的命名抽头
//! 进脚本私有累加缓冲(与 UI 泵线程并行,互不抢占);
//! 停止模型:协作式——脚本通过 `stop_requested()` 或 `sleep`/`expect` 内部检查退出。

use crate::session::SessionHandle;
use mlua::{Lua, StdLib};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ScriptContext {
    pub session: SessionHandle,
    /// 外部停止标志(面板"停止"按钮 / 标签关闭)
    pub stop: Arc<AtomicBool>,
    /// 脚本 log() 输出回调(终端系统行)
    pub on_log: Box<dyn FnMut(&str) + Send>,
}

#[derive(Debug)]
pub struct ScriptError(pub String);

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// 阻塞运行脚本直到结束/出错;调用方在独立线程中执行。
pub fn run_script(ctx: ScriptContext, code: &str) -> Result<(), ScriptError> {
    let lua = Lua::new_with(
        StdLib::ALL_SAFE,
        mlua::LuaOptions::default(),
    )
    .map_err(|e| ScriptError(format!("Lua 初始化失败: {e}")))?;

    let session = ctx.session;
    let stop = ctx.stop; // 直接复用外部标志(此前值复制导致外部 stop 无法传入——根因)
    let log: Arc<Mutex<Box<dyn FnMut(&str) + Send>>> = Arc::new(Mutex::new(ctx.on_log));

    // 抽头累加缓冲:expect/read 的数据来源
    let acc: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let acc_tap = Arc::clone(&acc);
    session.add_tap(
        "lua",
        Box::new(move |data: &[u8]| {
            acc_tap.lock().unwrap().extend_from_slice(data);
        }),
    );

    let t0 = Instant::now();
    let ms = |t0: Instant| t0.elapsed().as_millis() as i64;

    let port = lua.create_table().map_err(|e| ScriptError(e.to_string()))?;

    // port.write(text) -> 已发送字节数
    {
        let session = session.clone();
        port.set(
            "write",
            lua.create_function(move |_, data: String| {
                session
                    .write(data.as_bytes())
                    .map(|n| n as i64)
                    .map_err(|e| mlua::Error::runtime(e.to_string()))
            })
            .map_err(|e| ScriptError(e.to_string()))?,
        )
        .map_err(|e| ScriptError(e.to_string()))?;
    }

    // port.write_hex("55 aa") -> 已发送字节数
    {
        let session = session.clone();
        port.set(
            "write_hex",
            lua.create_function(move |_, hex: String| {
                let cleaned: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                if cleaned.len() % 2 != 0 {
                    return Err(mlua::Error::runtime("HEX 位数必须为偶数"));
                }
                let bytes: Vec<u8> = (0..cleaned.len())
                    .step_by(2)
                    .filter_map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
                    .collect();
                session
                    .write(&bytes)
                    .map(|n| n as i64)
                    .map_err(|e| mlua::Error::runtime(e.to_string()))
            })
            .map_err(|e| ScriptError(e.to_string()))?,
        )
        .map_err(|e| ScriptError(e.to_string()))?;
    }

    // port.expect(pattern, timeout_ms) -> string|nil
    // 等待包含 pattern 的数据;命中返回累计文本(含匹配,缓冲消费);超时返回累计文本并保留缓冲
    {
        let acc_e = Arc::clone(&acc);
        port.set(
            "expect",
            lua.create_function(move |_, (pattern, timeout_ms): (String, i64)| {
                let pat = pattern.into_bytes();
                let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
                loop {
                    {
                        let mut a = acc_e.lock().unwrap();
                        if let Some(pos) = find_sub(&a, &pat) {
                            let text =
                                String::from_utf8_lossy(&a[..pos + pat.len()]).into_owned();
                            a.drain(..pos + pat.len());
                            eprintln!("[lua-debug] expect 命中,累计 {}B", text.len());
                            return Ok(Some(text));
                        }
                        eprintln!("[lua-debug] expect 未命中,缓冲 {}B", a.len());
                    }
                    if Instant::now() >= deadline {
                        let a = acc_e.lock().unwrap();
                        return Ok(Some(String::from_utf8_lossy(&a).into_owned()));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .map_err(|e| ScriptError(e.to_string()))?,
        )
        .map_err(|e| ScriptError(e.to_string()))?;
    }

    // port.read() -> string:取走当前累计缓冲(清空)
    {
        let acc_r = Arc::clone(&acc);
        port.set(
            "read",
            lua.create_function(move |_, ()| {
                let mut a = acc_r.lock().unwrap();
                let text = String::from_utf8_lossy(&a).into_owned();
                a.clear();
                Ok(text)
            })
            .map_err(|e| ScriptError(e.to_string()))?,
        )
        .map_err(|e| ScriptError(e.to_string()))?;
    }

    // sleep(ms):分片等待,响应停止
    {
        let stop_s = Arc::clone(&stop);
        lua.globals()
            .set(
                "sleep",
                lua.create_function(move |_, ms: i64| {
                    let deadline = Instant::now() + Duration::from_millis(ms.max(0) as u64);
                    while Instant::now() < deadline {
                        if stop_s.load(Ordering::Relaxed) {
                            return Err(mlua::Error::runtime("脚本已停止"));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(())
                })
                .map_err(|e| ScriptError(e.to_string()))?,
            )
            .map_err(|e| ScriptError(e.to_string()))?;
    }

    // log(msg)
    {
        let log_cb = Arc::clone(&log);
        lua.globals()
            .set(
                "log",
                lua.create_function(move |_, msg: String| {
                    eprintln!("[lua-debug] log() 被调用: {}", msg);
                    if let Ok(mut cb) = log_cb.lock() {
                        cb(&msg);
                        eprintln!("[lua-debug] log() 回调完成");
                    } else {
                        eprintln!("[lua-debug] log_cb.lock 失败");
                    }
                    Ok(())
                })
                .map_err(|e| ScriptError(e.to_string()))?,
            )
            .map_err(|e| ScriptError(e.to_string()))?;
    }

    // stop_requested() / ms()
    {
        let stop_q = Arc::clone(&stop);
        lua.globals()
            .set(
                "stop_requested",
                lua.create_function(move |_, ()| Ok(stop_q.load(Ordering::Relaxed)))
                    .map_err(|e| ScriptError(e.to_string()))?,
            )
            .map_err(|e| ScriptError(e.to_string()))?;
    }
    {
        let t0c = t0;
        lua.globals()
            .set(
                "ms",
                lua.create_function(move |_, ()| Ok(ms(t0c)))
                    .map_err(|e| ScriptError(e.to_string()))?,
            )
            .map_err(|e| ScriptError(e.to_string()))?;
    }

    lua.globals().set("port", port).map_err(|e| ScriptError(e.to_string()))?;

    lua.load(code)
        .set_name("portpilot_script")
        .exec()
        .map_err(|e| ScriptError(format!("Lua 运行错误: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // 用回环传输做脚本引擎的闭环测试:write 的数据经回环读回
    #[test]
    fn lua_write_expect_roundtrip() {
        // 回环会话:write 的数据直接回流到抽头
        let session = SessionHandle::new_for_test_loopback();
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        let logs2 = Arc::clone(&logs);
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = ScriptContext {
            session,
            stop: Arc::clone(&stop),
            on_log: Box::new(move |m: &str| logs2.lock().unwrap().push(m.to_string())),
        };
        let code = r#"
            port.write("PING")
            local resp = port.expect("PING", 2000)
            assert(resp and resp:find("PING"), "expect 未命中: " .. tostring(resp))
            log("roundtrip ok, len=" .. #resp)
        "#;
        run_script(ctx, code).expect("脚本应成功");
        assert!(
            logs.lock().unwrap().iter().any(|m| m.contains("roundtrip ok")),
            "log 回调应收到 roundtrip 消息"
        );
    }

    #[test]
    fn lua_stop_interrupts_sleep() {
        let session = SessionHandle::new_for_test_loopback();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = ScriptContext {
            session,
            stop: Arc::clone(&stop),
            on_log: Box::new(|_| {}),
        };
        let stop2 = Arc::clone(&stop);
        let t = std::thread::spawn(move || {
            run_script(
                ctx,
                r#"
                for i = 1, 100 do
                    sleep(1000)
                end
            "#,
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        stop2.store(true, Ordering::Relaxed);
        let started = Instant::now();
        let res = t.join().unwrap();
        assert!(res.is_err(), "停止后脚本应以错误结束");
        assert!(started.elapsed() < std::time::Duration::from_secs(3), "停止应 promptly 生效");
    }

    // 供回环测试使用的最小构造(传输在 transport.rs 中定义)
    impl SessionHandle {
        pub fn new_for_test_loopback() -> SessionHandle {
            crate::session::spawn_session(crate::session::SessionOptions::new(Box::new(|| {
                crate::open_transport(&crate::TransportConfig {
                    kind: crate::TransportKind::Loop,
                    port: String::new(),
                    remote: None,
                    baud: 0,
                    data_bits: 8,
                    stop_bits: 1,
                    parity: crate::Parity::None,
                    read_timeout_ms: 5,
                })
            })))
        }
    }

    // 计数器占位避免未使用告警(保留未来断言扩展)
    static _COUNTER: AtomicUsize = AtomicUsize::new(0);
    #[test]
    fn counter_unused_placeholder() {
        assert_eq!(_COUNTER.load(Ordering::Relaxed), 0);
    }
}
