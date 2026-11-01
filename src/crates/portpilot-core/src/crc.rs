//! 校验和计算:常用 CRC 目录 + 累加和/异或。
//! 标准测试向量均为 "123456789" 的公认值(见 tests)。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumKind {
    None,
    Sum8,
    Xor8,
    Crc16Modbus,
    Crc16CcittFalse,
    Crc16Xmodem,
    Crc32,
}

impl ChecksumKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "sum8" => Self::Sum8,
            "xor8" => Self::Xor8,
            "crc16_modbus" => Self::Crc16Modbus,
            "crc16_ccitt" => Self::Crc16CcittFalse,
            "crc16_xmodem" => Self::Crc16Xmodem,
            "crc32" => Self::Crc32,
            _ => Self::None,
        }
    }
    pub fn len_bytes(&self) -> usize {
        match self {
            Self::Sum8 | Self::Xor8 => 1,
            Self::Crc16Modbus | Self::Crc16CcittFalse | Self::Crc16Xmodem => 2,
            Self::Crc32 => 4,
            Self::None => 0,
        }
    }
}

pub fn sum8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

pub fn xor8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a ^ b)
}

/// 反射 CRC16(MODBUS poly=0xA001 init=0xFFFF)
pub fn crc16_modbus(data: &[u8]) -> u16 {
    crc16_reflected(data, 0xA001, 0xFFFF)
}

fn crc16_reflected(data: &[u8], poly: u16, init: u16) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ poly } else { crc >> 1 };
        }
    }
    crc
}

/// 非反射 CRC16(poly=0x1021),init 可变:CCITT-FALSE=0xFFFF,XMODEM=0x0000
fn crc16_msb(data: &[u8], init: u16) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    crc16_msb(data, 0xFFFF)
}

pub fn crc16_xmodem(data: &[u8]) -> u16 {
    crc16_msb(data, 0x0000)
}

pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// 把校验和按字节序追加到 payload 尾部。
pub fn append_checksum(payload: &mut Vec<u8>, kind: ChecksumKind, little_endian: bool) {
    match kind {
        ChecksumKind::None => {}
        ChecksumKind::Sum8 => payload.push(sum8(payload)),
        ChecksumKind::Xor8 => payload.push(xor8(payload)),
        ChecksumKind::Crc16Modbus => {
            let c = crc16_modbus(payload).to_le_bytes();
            let bytes = if little_endian { c } else { [c[1], c[0]] };
            payload.extend_from_slice(&bytes);
        }
        ChecksumKind::Crc16CcittFalse => {
            let c = crc16_ccitt_false(payload).to_le_bytes();
            let bytes = if little_endian { c } else { [c[1], c[0]] };
            payload.extend_from_slice(&bytes);
        }
        ChecksumKind::Crc16Xmodem => {
            let c = crc16_xmodem(payload).to_le_bytes();
            let bytes = if little_endian { c } else { [c[1], c[0]] };
            payload.extend_from_slice(&bytes);
        }
        ChecksumKind::Crc32 => {
            let c = crc32_ieee(payload).to_le_bytes();
            let bytes = if little_endian { c } else { [c[3], c[2], c[1], c[0]] };
            payload.extend_from_slice(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &[u8] = b"123456789";

    #[test]
    fn crc16_modbus_vector() {
        assert_eq!(crc16_modbus(V), 0x4B37);
    }
    #[test]
    fn crc16_ccitt_false_vector() {
        assert_eq!(crc16_ccitt_false(V), 0x29B1);
    }
    #[test]
    fn crc16_xmodem_vector() {
        assert_eq!(crc16_xmodem(V), 0x31C3);
    }
    #[test]
    fn crc32_vector() {
        assert_eq!(crc32_ieee(V), 0xCBF4_3926);
    }
    #[test]
    fn sum_xor_basic() {
        assert_eq!(sum8(V), (V.iter().map(|b| *b as u32).sum::<u32>() & 0xFF) as u8);
        let mut x = 0u8;
        for b in V {
            x ^= b;
        }
        assert_eq!(xor8(V), x);
    }
    #[test]
    fn append_endian() {
        let mut p = b"12".to_vec();
        append_checksum(&mut p, ChecksumKind::Crc16Modbus, false);
        assert_eq!(p.len(), 4);
        // 大端:高字节在前
        let c = crc16_modbus(b"12").to_be_bytes();
        assert_eq!(&p[2..], &c);
    }
}
