//! 传输抽象:`Transport` trait + 串口/TCP/UDP 实现 + 枚举。
//!
//! UI/业务层只面向本模块的 trait,永不直接触碰平台 API;
//! 网络传输复用会话层的重连/环形缓冲/规则引擎,零额外成本;
//! 测试用自定义 Transport 工厂注入假传输(见 session.rs 的测试)。

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    None,
    Odd,
    Even,
}

/// 传输类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Serial,
    TcpClient,
    TcpServer,
    Udp,
    /// 回环自测:write 的数据从 read 读回(无硬件演示/自检)
    Loop,
}

impl TransportKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "tcp_client" => Self::TcpClient,
            "tcp_server" => Self::TcpServer,
            "udp" => Self::Udp,
            "loop" => Self::Loop,
            _ => Self::Serial,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub kind: TransportKind,
    /// 串口:"COM9";TCP 客户端:"host:port";TCP 服务端/UDP:本地绑定 "0.0.0.0:port"
    pub port: String,
    /// UDP 发送目标(可选;未配置时回复最后一个来源)
    pub remote: Option<String>,
    pub baud: u32,     // 仅串口
    pub data_bits: u8, // 5..=8
    pub stop_bits: u8, // 1 | 2
    pub parity: Parity,
    pub read_timeout_ms: u64,
}

impl TransportConfig {
    pub fn new(port: impl Into<String>, baud: u32) -> Self {
        Self {
            kind: TransportKind::Serial,
            port: port.into(),
            remote: None,
            baud,
            data_bits: 8,
            stop_bits: 1,
            parity: Parity::None,
            read_timeout_ms: 5,
        }
    }
}

/// 传输层抽象:阻塞读 + 写 + 关闭 + 线控。实现必须 Send。
pub trait Transport: Send {
    /// 阻塞读;`Ok(0)` 表示超时无数据(调用方继续轮询)。
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, data: &[u8]) -> io::Result<usize>;
    fn close(&mut self);
    /// DTR 线控(复位板卡刚需);不支持时默认无操作。
    fn set_dtr(&mut self, _on: bool) {}
    /// RTS 线控。
    fn set_rts(&mut self, _on: bool) {}
}

/// 串口信息(枚举结果)。
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub name: String,
    pub usb_vid: Option<u16>,
    pub usb_pid: Option<u16>,
}

/// 枚举系统串口(含 USB VID/PID,用于设备友好显示)。
pub fn list_ports() -> Vec<PortInfo> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| match &p.port_type {
            serialport::SerialPortType::UsbPort(info) => PortInfo {
                name: p.port_name.clone(),
                usb_vid: Some(info.vid),
                usb_pid: Some(info.pid),
            },
            _ => PortInfo { name: p.port_name.clone(), usb_vid: None, usb_pid: None },
        })
        .collect()
}

/// 按配置打开传输(串口/TCP/UDP 统一入口)。
pub fn open_transport(cfg: &TransportConfig) -> io::Result<Box<dyn Transport>> {
    match cfg.kind {
        TransportKind::Serial => open_serial(cfg),
        TransportKind::TcpClient => Ok(Box::new(TcpClientTransport::open(&cfg.port)?)),
        TransportKind::TcpServer => Ok(Box::new(TcpServerTransport::open(&cfg.port)?)),
        TransportKind::Udp => Ok(Box::new(UdpTransport::open(
            &cfg.port,
            cfg.remote.as_deref(),
        )?)),
        TransportKind::Loop => Ok(Box::new(LoopbackTransport::new())),
    }
}

/// 打开真实串口,返回 `Box<dyn Transport>`。
pub fn open_serial(cfg: &TransportConfig) -> io::Result<Box<dyn Transport>> {
    let parity = match cfg.parity {
        Parity::None => serialport::Parity::None,
        Parity::Odd => serialport::Parity::Odd,
        Parity::Even => serialport::Parity::Even,
    };
    let port = serialport::new(&cfg.port, cfg.baud)
        .data_bits(match cfg.data_bits {
            5 => serialport::DataBits::Five,
            6 => serialport::DataBits::Six,
            7 => serialport::DataBits::Seven,
            _ => serialport::DataBits::Eight,
        })
        .stop_bits(if cfg.stop_bits == 2 {
            serialport::StopBits::Two
        } else {
            serialport::StopBits::One
        })
        .parity(parity)
        .timeout(Duration::from_millis(cfg.read_timeout_ms.max(1)))
        .open()
        .map_err(io::Error::other)?;
    Ok(Box::new(SerialTransport { inner: Some(port) }))
}

struct SerialTransport {
    // Some = 打开;close 后置 None(幂等关闭)
    inner: Option<Box<dyn serialport::SerialPort>>,
}

/// 串口读超时归一化:serialport crate 在超时时返回 Err(TimedOut),
/// 而 Transport 契约是"Ok(0) 表示超时无数据"(session 线程据此继续轮询,
/// 把 TimedOut 当错误会触发断线重连风暴——静默链路实测踩坑,见 docs/03 坑 17)。
fn normalize_serial_read(r: io::Result<usize>) -> io::Result<usize> {
    match r {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
        Err(e) => Err(e),
    }
}

impl Transport for SerialTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.as_mut() {
            Some(p) => normalize_serial_read(p.read(buf)),
            None => Ok(0),
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.inner.as_mut() {
            Some(p) => p.write(data),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "端口已关闭")),
        }
    }

    fn close(&mut self) {
        self.inner = None; // Drop 释放句柄
    }

    fn set_dtr(&mut self, on: bool) {
        if let Some(p) = self.inner.as_mut() {
            let _ = p.write_data_terminal_ready(on);
        }
    }

    fn set_rts(&mut self, on: bool) {
        if let Some(p) = self.inner.as_mut() {
            let _ = p.write_request_to_send(on);
        }
    }
}

// ============ TCP 客户端 ============

pub struct TcpClientTransport {
    stream: Option<TcpStream>,
    addr: String,
}

impl TcpClientTransport {
    pub fn open(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        // 50ms 读超时:Windows 上超时表现为 read 返回 Ok(0)(非 Err),
        // 与 Unix 习惯不同;空闲唤醒 20 次/秒,可接受
        stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        stream.set_nodelay(true).ok();
        Ok(Self { stream: Some(stream), addr: addr.to_string() })
    }
}

impl Transport for TcpClientTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stream.as_mut() {
            Some(s) => match s.read(buf) {
                // Windows:超时 = Ok(0);Err(TimedOut/WouldBlock) 同样归一为无数据。
                // 注意:对端优雅关闭(FIN)也是 Ok(0),与超时不可区分——
                // 断线检测依赖 Err(ConnectionReset/Aborted)与发送侧失败,静默视为空闲。
                Err(e) if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut => Ok(0),
                Ok(0) => Ok(0),
                n => n,
            },
            None => Ok(0),
        }
    }
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.stream.as_mut() {
            Some(s) => s.write(data),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "未连接")),
        }
    }
    fn close(&mut self) {
        self.stream = None;
    }
}

// ============ TCP 服务端(单客户端;断开后重新监听等待) ============

pub struct TcpServerTransport {
    listener: Option<TcpListener>,
    stream: Option<TcpStream>,
    addr: String,
    idle_ok0: u32,
}

impl TcpServerTransport {
    pub fn open(bind: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener: Some(listener), stream: None, addr: bind.to_string(), idle_ok0: 0 })
    }

    pub fn local_addr(&self) -> io::Result<String> {
        match self.listener.as_ref() {
            Some(l) => Ok(l.local_addr()?.to_string()),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "未监听")),
        }
    }

    /// 无连接时非阻塞 accept 轮询,直到超时窗口结束(模拟读超时语义)。
    fn accept_poll(&mut self) -> io::Result<usize> {
        let deadline = Instant::now() + Duration::from_millis(150);
        loop {
            if let Some(l) = self.listener.as_ref() {
                match l.accept() {
                    Ok((s, _)) => {
                        s.set_read_timeout(Some(Duration::from_millis(50)))?;
                        s.set_nodelay(true).ok();
                        self.stream = Some(s);
                        self.idle_ok0 = 0;
                        return Ok(0); // 本轮先返回,下轮开始读数据
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            if Instant::now() >= deadline {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Transport for TcpServerTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.stream.is_none() {
            return self.accept_poll();
        }
        let res = self.stream.as_mut().unwrap().read(buf);
        match res {
            // Windows:超时 = Ok(0);对端优雅关闭也是 Ok(0)——
            // 连续 30s 无数据则回收连接重新监听( Err 类错误仍立即断开)
            Err(e) if e.kind() == io::ErrorKind::WouldBlock
                || e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Ok(0) => {
                self.idle_ok0 += 1;
                if self.idle_ok0 > 600 {
                    self.idle_ok0 = 0;
                    self.stream = None; // 回到监听态
                }
                Ok(0)
            }
            other => {
                self.idle_ok0 = 0;
                other
            }
        }
    }
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.stream.as_mut() {
            Some(s) => s.write(data),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "暂无客户端连接")),
        }
    }
    fn close(&mut self) {
        self.stream = None;
        self.listener = None;
    }
}

// ============ 回环自测(write 数据从 read 读回) ============

pub struct LoopbackTransport {
    pending: Arc<Mutex<Vec<u8>>>,
}

impl LoopbackTransport {
    pub fn new() -> Self {
        Self { pending: Arc::new(Mutex::new(Vec::new())) }
    }
}

impl Default for LoopbackTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for LoopbackTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut p = self.pending.lock().unwrap();
        if p.is_empty() {
            // 空转休眠:会话读循环无超时语义,避免 100% CPU
            std::thread::sleep(Duration::from_millis(5));
            return Ok(0);
        }
        let n = buf.len().min(p.len());
        buf[..n].copy_from_slice(&p[..n]);
        p.drain(..n);
        Ok(n)
    }
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.pending.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn close(&mut self) {}
}

// ============ UDP(绑定本地端口收;发送到 remote 或最后一个来源) ============

pub struct UdpTransport {
    socket: UdpSocket,
    remote: Option<String>,
    last_peer: Option<std::net::SocketAddr>,
}

impl UdpTransport {
    pub fn open(bind: &str, remote: Option<&str>) -> io::Result<Self> {
        let bind_addr = if bind.contains(':') {
            bind.to_string()
        } else {
            format!("0.0.0.0:{}", bind)
        };
        let socket = UdpSocket::bind(&bind_addr)?;
        socket.set_read_timeout(Some(Duration::from_millis(5)))?;
        Ok(Self { socket, remote: remote.map(String::from), last_peer: None })
    }
}

impl Transport for UdpTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.socket.recv_from(buf) {
            Ok((n, peer)) => {
                self.last_peer = Some(peer);
                Ok(n)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock
                || e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let target = match (&self.remote, self.last_peer) {
            (Some(r), _) => r.to_socket_addrs()?.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "remote 地址无效")
            })?,
            (None, Some(p)) => p,
            (None, None) => {
                return Err(io::Error::new(io::ErrorKind::NotConnected, "UDP 无发送目标"));
            }
        };
        self.socket.send_to(data, target)
    }
    fn close(&mut self) {
        // UDP 无连接语义;socket 随 Drop 释放,会话重连由工厂重新 bind
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn config_defaults() {
        let c = TransportConfig::new("COM9", 460800);
        assert_eq!(c.port, "COM9");
        assert_eq!(c.baud, 460800);
        assert_eq!(c.data_bits, 8);
        assert_eq!(c.stop_bits, 1);
        assert_eq!(c.parity, Parity::None);
    }

    #[test]
    fn list_ports_does_not_panic() {
        // 无硬件也必须安全返回(可能为空)
        let _ = list_ports();
    }

    #[test]
    fn serial_read_timeout_normalizes_to_ok0() {
        // 坑 17:serialport crate 超时返回 Err(TimedOut),必须归一化为 Ok(0),
        // 否则静默链路(雷达等)会触发断线重连风暴
        let err = io::Error::from(io::ErrorKind::TimedOut);
        assert_eq!(normalize_serial_read(Ok(0)).unwrap(), 0);
        assert_eq!(normalize_serial_read(Ok(7)).unwrap(), 7);
        assert_eq!(normalize_serial_read(Err(err)).unwrap(), 0);
        let other = io::Error::from(io::ErrorKind::NotFound);
        assert!(normalize_serial_read(Err(other)).is_err());
    }

    #[test]
    fn tcp_client_reads_from_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let t = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut s = s;
            s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
            s.write_all(b"hello-tcp").unwrap();
            let mut b = [0u8; 64];
            let n = s.read(&mut b).unwrap_or(0);
            if n > 0 {
                s.write_all(&b[..n]).unwrap();
            }
        });
        let cfg = TransportConfig {
            kind: TransportKind::TcpClient,
            port: addr,
            remote: None,
            baud: 0,
            data_bits: 8,
            stop_bits: 1,
            parity: Parity::None,
            read_timeout_ms: 50,
        };
        let mut tp = open_transport(&cfg).unwrap();
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while got.len() < 9 && Instant::now() < deadline {
            let mut b = [0u8; 64];
            match tp.read(&mut b) {
                Ok(n) if n > 0 => got.extend_from_slice(&b[..n]),
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(&got, b"hello-tcp");
        let _ = tp.write(b"ping");
        t.join().unwrap();
    }

    #[test]
    fn tcp_server_receives_from_client() {
        let mut srv = TcpServerTransport::open("127.0.0.1:0").unwrap();
        let addr = srv.local_addr().unwrap();
        let t = thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(b"ping-udp-like").unwrap();
            thread::sleep(Duration::from_millis(300));
        });
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while got.len() < 13 && Instant::now() < deadline {
            let mut b = [0u8; 64];
            match srv.read(&mut b) {
                Ok(n) if n > 0 => got.extend_from_slice(&b[..n]),
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(&got, b"ping-udp-like");
        t.join().unwrap();
    }

    #[test]
    fn tcp_read_timeout_is_ok0_not_err() {
        // 回归:std TCP 读超时返回 Err(TimedOut),必须归一为 Ok(0),
        // 否则会话层把静默当断线(实测:P1 探针验证时静默 4s 触发重连风暴)
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let t = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(300)); // 故意静默 300ms
            drop(s);
        });
        let mut tp = TcpClientTransport::open(&addr).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut errs = 0;
        while Instant::now() < deadline {
            let mut b = [0u8; 64];
            match tp.read(&mut b) {
                Ok(0) => {}
                Err(e) => {
                    errs += 1;
                    panic!("静默期出现读错误 #{}: kind={:?} os={:?} msg={}", errs, e.kind(), e.raw_os_error(), e);
                }
                Ok(_) => {}
            }
            thread::sleep(Duration::from_millis(10));
        }
        t.join().unwrap();
    }

    #[test]
    fn udp_roundtrip() {
        let mut a = UdpTransport::open("127.0.0.1:0", None).unwrap();
        let b_port = {
            let s = UdpSocket::bind("127.0.0.1:0").unwrap();
            s.local_addr().unwrap().to_string()
        };
        let b = UdpSocket::bind(&b_port).unwrap();
        b.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        // a 发往 b(指定 remote)
        let mut a2 = UdpTransport::open("127.0.0.1:0", Some(&b_port)).unwrap();
        let _ = a2.write(b"udp-hello");
        let mut buf = [0u8; 64];
        let (n, _) = b.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"udp-hello");
        // b 回发,a 收(记录来源 peer)
        let _ = b.send_to(b"udp-reply", a.socket.local_addr().unwrap());
        let n = a.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"udp-reply");
        let _ = a2;
        let _ = a.socket.local_addr().unwrap();
    }
}
