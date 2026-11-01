//! 传输抽象:`Transport` trait + 串口实现 + 枚举。
//!
//! UI/业务层只面向本模块的 trait,永不直接触碰平台串口 API;
//! 测试用自定义 Transport 工厂注入假传输(见 session.rs 的测试)。

use std::io;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// 端口名,如 "COM9" / "/dev/ttyUSB0"
    pub port: String,
    pub baud: u32,
    pub data_bits: u8, // 5..=8
    pub stop_bits: u8, // 1 | 2
    pub parity: Parity,
    pub read_timeout_ms: u64,
}

impl TransportConfig {
    pub fn new(port: impl Into<String>, baud: u32) -> Self {
        Self {
            port: port.into(),
            baud,
            data_bits: 8,
            stop_bits: 1,
            parity: Parity::None,
            read_timeout_ms: 5,
        }
    }
}

/// 传输层抽象:阻塞读 + 写 + 关闭。实现必须 Send。
pub trait Transport: Send {
    /// 阻塞读;`Ok(0)` 表示超时无数据(调用方继续轮询)。
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, data: &[u8]) -> io::Result<usize>;
    fn close(&mut self);
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

impl Transport for SerialTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.as_mut() {
            Some(p) => p.read(buf),
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
