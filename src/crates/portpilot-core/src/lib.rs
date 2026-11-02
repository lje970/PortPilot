//! PortPilot 核心层 —— 平台无关、可单测、零 UI 依赖。
//!
//! 架构(规划书 §5.3):
//! ```text
//! UI(任意栈) ──> SessionHandle(drain 30fps) ──> SpscRing(免分配) <── I/O 线程(SerialTransport + 自动重连)
//! ```
//! 设计红线(M0/M1 实测得出):
//! 1. I/O 线程**零堆分配**(G1 实测:emit/分配引发的堆锁竞争会拖垮读线程导致丢包);
//! 2. 所有跨线程数据走固定容量 SPSC 环形缓冲,写侧仅在缓冲满时短暂自旋(背压);
//! 3. 传输层抽象为 trait,业务/UI 永不直接触碰串口句柄。

pub mod crc;
pub mod ring;
pub mod scope;
pub mod session;
pub mod stats;
pub mod transport;

pub use crc::{append_checksum, ChecksumKind};
pub use ring::SpscRing;
pub use scope::{make_parser, ScopeParser};
pub use session::{spawn_session, SessionHandle, SessionOptions, StatsSnapshot};
pub use stats::p95;
pub use transport::{
    list_ports, open_serial, open_transport, Parity, PortInfo, Transport, TransportConfig,
    TransportKind,
};
