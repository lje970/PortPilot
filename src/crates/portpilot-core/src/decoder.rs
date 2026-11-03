//! 配置驱动的协议帧解码器:帧头对齐 + 定长/帧尾切帧 + 字段表解码 + 校验验证。
//!
//! 约定:字段 offset 相对"帧头之后"的第一个字节(无帧头时相对帧首);
//! 校验和位于整帧尾部(按 ChecksumKind 长度),验证范围 = 帧(含头)去尾部校验。

use crate::crc::{self, ChecksumKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Uint,
    Int,
    Float,
    Bytes,
    Str,
}

impl FieldKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "int" => Self::Int,
            "float" => Self::Float,
            "bytes" => Self::Bytes,
            "str" => Self::Str,
            _ => Self::Uint,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    /// 相对帧头后第一字节的偏移
    pub offset: usize,
    /// 字节数;Str 类型填 0 表示"剩余全部"
    pub len: usize,
    pub kind: FieldKind,
    /// 多字节字段字节序(true = 小端)
    pub le: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecoderConfig {
    pub name: String,
    /// 帧头字节(HEX 文本由 app 层解析后传入);空 = 无帧头
    pub header: Vec<u8>,
    /// 定长帧:从帧头首字节算起的整帧长度(含头/尾/校验);None = 用帧尾切帧
    pub frame_len: Option<usize>,
    /// 帧尾字节(变长帧;与 frame_len 二选一)
    pub tail: Vec<u8>,
    pub fields: Vec<FieldDef>,
    /// 尾部校验类型(none 不验证)
    pub checksum: String,
}

impl DecoderConfig {
    pub fn checksum_kind(&self) -> ChecksumKind {
        ChecksumKind::from_str(&self.checksum)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodedFrame {
    pub ok: bool, // 校验通过(未启用校验恒 true)
    pub fields: Vec<(String, String)>,
    pub raw: Vec<u8>,
}

pub struct FrameDecoder {
    cfg: DecoderConfig,
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new(cfg: DecoderConfig) -> Self {
        Self { cfg, buf: Vec::new() }
    }

    pub fn config(&self) -> &DecoderConfig {
        &self.cfg
    }

    fn checksum_len(&self) -> usize {
        self.cfg.checksum_kind().len_bytes()
    }

    fn verify(&self, frame: &[u8]) -> bool {
        let kind = self.cfg.checksum_kind();
        if kind == ChecksumKind::None {
            return true;
        }
        let n = kind.len_bytes();
        if frame.len() < n {
            return false;
        }
        let body = &frame[..frame.len() - n];
        let stored = &frame[frame.len() - n..];
        let mut calc = body.to_vec();
        crc::append_checksum(&mut calc, kind, true); // 验证用小端拼接后比较整段
        calc[frame.len() - n..] == *stored
    }

    fn decode_fields(&self, frame: &[u8]) -> Vec<(String, String)> {
        let base = self.cfg.header.len().min(frame.len());
        let ck = self.checksum_len();
        // body = 帧头之后、校验之前的区域;字段 offset/len 均相对 body
        let body = &frame[base..frame.len().saturating_sub(ck)];
        let mut out = Vec::new();
        for f in &self.cfg.fields {
            let s = f.offset.min(body.len());
            let e = if f.kind == FieldKind::Str && f.len == 0 {
                body.len() // Str len=0:剩余全部
            } else {
                (s + f.len).min(body.len())
            };
            let v = if s >= e {
                "?".into()
            } else {
                let b = &body[s..e];
                match f.kind {
                    FieldKind::Uint => {
                        let src: Vec<u8> = if f.le { b.iter().rev().copied().collect() } else { b.to_vec() };
                        let mut be = [0u8; 16];
                        be[16 - src.len()..].copy_from_slice(&src);
                        u128::from_be_bytes(be).to_string()
                    }
                    FieldKind::Int => {
                        let src: Vec<u8> = if f.le { b.iter().rev().copied().collect() } else { b.to_vec() };
                        let neg = !src.is_empty() && src[0] & 0x80 != 0;
                        let mut be = [0u8; 16];
                        be[..16 - src.len()].fill(if neg { 0xFF } else { 0x00 });
                        be[16 - src.len()..].copy_from_slice(&src);
                        i128::from_be_bytes(be).to_string()
                    }
                    FieldKind::Float => match b.len() {
                        4 => format!("{:.4}", f32::from_le_bytes(b.try_into().unwrap())),
                        8 => format!("{:.6}", f64::from_le_bytes(b.try_into().unwrap())),
                        _ => "?(float 4/8B)".into(),
                    },
                    FieldKind::Bytes => b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "),
                    FieldKind::Str => String::from_utf8_lossy(b).into_owned(),
                }
            };
            out.push((f.name.clone(), v));
        }
        out
    }

    fn frame_out(&self, frame: Vec<u8>) -> DecodedFrame {
        let ok = self.verify(&frame);
        let fields = self.decode_fields(&frame);
        DecodedFrame { ok, fields, raw: frame }
    }

    /// 喂入原始字节,返回解析出的帧。
    pub fn feed(&mut self, data: &[u8]) -> Vec<DecodedFrame> {
        self.buf.extend_from_slice(data);
        if self.buf.len() > 4 * 1024 * 1024 {
            self.buf.clear(); // 防御:异常流不无限吃内存
        }
        let mut out = Vec::new();
        let header = self.cfg.header.clone();
        let tail = self.cfg.tail.clone();
        loop {
            // 1) 帧头对齐
            if !header.is_empty() {
                match find_sub(&self.buf, &header) {
                    Some(h) => {
                        self.buf.drain(..h);
                    }
                    None => {
                        // 保留末尾 header.len()-1 字节防帧头被切断,其余丢弃
                        let keep = header.len().saturating_sub(1);
                        if self.buf.len() > keep {
                            self.buf.drain(..self.buf.len() - keep);
                        }
                        break;
                    }
                }
            }
            // 2) 取帧
            if let Some(fl) = self.cfg.frame_len {
                if self.buf.len() < fl {
                    break;
                }
                let frame = self.buf[..fl].to_vec();
                self.buf.drain(..fl);
                out.push(self.frame_out(frame));
            } else if !tail.is_empty() {
                match find_sub(&self.buf[header.len().min(self.buf.len())..], &tail) {
                    Some(t) => {
                        let end = header.len() + t + tail.len();
                        let frame = self.buf[..end].to_vec();
                        self.buf.drain(..end);
                        out.push(self.frame_out(frame));
                    }
                    None => break,
                }
            } else {
                break; // 既无定长也无帧尾:无法切帧
            }
            if out.len() > 4096 {
                self.buf.clear();
                break;
            }
        }
        out
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// 内置解码模板:NMEA-0183 行(文本)与 Modbus RTU 定长 8B 示例。
pub fn builtin_templates() -> Vec<DecoderConfig> {
    vec![
        DecoderConfig {
            name: "NMEA-0183".into(),
            header: b"$".to_vec(),
            frame_len: None,
            tail: b"\n".to_vec(),
            fields: vec![FieldDef {
                name: "sentence".into(),
                offset: 0,
                len: 0,
                kind: FieldKind::Str,
                le: false,
            }],
            checksum: "none".into(),
        },
        DecoderConfig {
            name: "Modbus RTU(8B 定长示例)".into(),
            header: vec![],
            frame_len: Some(8),
            tail: vec![],
            fields: vec![
                FieldDef { name: "addr".into(), offset: 0, len: 1, kind: FieldKind::Uint, le: false },
                FieldDef { name: "func".into(), offset: 1, len: 1, kind: FieldKind::Uint, le: false },
                FieldDef { name: "value".into(), offset: 2, len: 2, kind: FieldKind::Uint, le: true },
            ],
            checksum: "crc16_modbus".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_cfg() -> DecoderConfig {
        // 帧结构:[AA 55][addr@0][value@1 LE][temp@3 f32 LE] = 9B(字段 offset 相对帧头后)
        DecoderConfig {
            name: "fixed9".into(),
            header: vec![0xAA, 0x55],
            frame_len: Some(9),
            tail: vec![],
            fields: vec![
                FieldDef { name: "addr".into(), offset: 0, len: 1, kind: FieldKind::Uint, le: false },
                FieldDef { name: "value".into(), offset: 1, len: 2, kind: FieldKind::Uint, le: true },
                FieldDef { name: "temp".into(), offset: 3, len: 4, kind: FieldKind::Float, le: true },
            ],
            checksum: "none".into(),
        }
    }

    #[test]
    fn fixed_frame_with_header_sync_and_garbage_prefix() {
        let mut d = FrameDecoder::new(fixed_cfg());
        let mut stream = vec![0x99, 0x98]; // 垃圾前缀
        let mut frame = vec![0xAA, 0x55, 0x01, 0xCD, 0xAB];
        frame.extend_from_slice(&12.5f32.to_le_bytes());
        stream.extend_from_slice(&frame);
        let out = d.feed(&stream);
        assert_eq!(out.len(), 1);
        assert!(out[0].ok);
        assert_eq!(out[0].fields[0], ("addr".into(), "1".into()));
        assert_eq!(out[0].fields[1], ("value".into(), "43981".into())); // 0xABCD LE
        assert_eq!(out[0].fields[2], ("temp".into(), "12.5000".into()));
    }

    #[test]
    fn split_across_batches() {
        let mut d = FrameDecoder::new(fixed_cfg());
        let frame = vec![0xAA, 0x55, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let out = d.feed(&frame[..3]);
        assert!(out.is_empty());
        let out = d.feed(&frame[3..]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].fields[0].1, "2");
    }

    #[test]
    fn checksum_bad_frame_marked() {
        let mut cfg = fixed_cfg();
        cfg.checksum = "sum8".into();
        let mut d = FrameDecoder::new(cfg);
        let mut frame = vec![0xAA, 0x55, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00];
        frame[8] = 0xFF; // 错误校验(尾字节)
        let out = d.feed(&frame);
        assert_eq!(out.len(), 1);
        assert!(!out[0].ok, "校验错误的帧应标记 ok=false");
    }

    #[test]
    fn tail_mode_nmea() {
        let mut d = FrameDecoder::new(builtin_templates()[0].clone());
        let out = d.feed(b"garbage\r\n$GPGGA,123456.00\r\n$GPGLL,x\r\n");
        assert_eq!(out.len(), 2);
        assert!(out[0].fields[0].1.starts_with("GPGGA"));
        assert!(out[1].fields[0].1.starts_with("GPGLL"));
    }

    #[test]
    fn crc16_modbus_frame_verify() {
        let mut d = FrameDecoder::new(DecoderConfig {
            name: "mb".into(),
            header: vec![],
            frame_len: Some(8),
            tail: vec![],
            fields: vec![FieldDef { name: "addr".into(), offset: 0, len: 1, kind: FieldKind::Uint, le: false }],
            checksum: "crc16_modbus".into(),
        });
        let mut frame = vec![0x01, 0x03, 0x00, 0x00, 0x00, 0x00];
        let mut calc = frame.clone();
        crc::append_checksum(&mut calc, ChecksumKind::Crc16Modbus, true);
        frame.extend_from_slice(&calc[6..8]); // calc = body+crc
        let out = d.feed(&frame);
        assert_eq!(out.len(), 1);
        assert!(out[0].ok, "正确 CRC 的帧应通过");
        // 翻转一位 CRC → 失败
        let mut bad = frame.clone();
        bad[7] ^= 0xFF;
        let out = d.feed(&bad);
        assert!(!out[0].ok);
    }
}
