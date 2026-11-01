//! 示波器数据协议解析:JustFloat(二进制)与 CSV 行(兼容 VOFA+ FireWater 习惯)。

pub const JUSTFLOAT_TAIL: [u8; 4] = [0x00, 0x00, 0x80, 0x7F];

pub trait ScopeParser: Send {
    /// 喂入原始字节,返回解析出的完整帧(每帧 = 各通道 f32)。
    fn feed(&mut self, data: &[u8]) -> Vec<Vec<f32>>;
}

/// JustFloat:N 个 f32(LE)+ 4 字节尾 {00 00 80 7F}。channels 由 UI 指定。
pub struct JustFloatParser {
    buf: Vec<u8>,
    channels: usize,
}

impl JustFloatParser {
    pub fn new(channels: usize) -> Self {
        Self { buf: Vec::new(), channels: channels.clamp(1, 32) }
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

impl ScopeParser for JustFloatParser {
    fn feed(&mut self, data: &[u8]) -> Vec<Vec<f32>> {
        self.buf.extend_from_slice(data);
        let frame_len = self.channels * 4 + 4;
        let mut out = Vec::new();
        loop {
            match find_sub(&self.buf, &JUSTFLOAT_TAIL) {
                Some(t) if t + 4 >= frame_len => {
                    let start = t + 4 - frame_len;
                    let floats: Vec<f32> = (0..self.channels)
                        .map(|c| {
                            let b: [u8; 4] = self.buf[start + c * 4..start + c * 4 + 4]
                                .try_into()
                                .unwrap();
                            f32::from_le_bytes(b)
                        })
                        .collect();
                    self.buf.drain(..t + 4);
                    out.push(floats);
                }
                // 尾前不足一帧:未对齐,丢弃到尾后重新同步
                Some(t) => {
                    self.buf.drain(..t + 4);
                }
                None => break,
            }
            if self.buf.len() > 4 * 1024 * 1024 {
                self.buf.clear(); // 防御:异常流不无限吃内存
                break;
            }
        }
        out
    }
}

/// CSV 行:每行逗号/空白分隔的浮点数,如 "1.0,2.5,-3\n"(兼容 FireWater)。
pub struct LineFloatParser {
    pending: Vec<u8>,
}

impl LineFloatParser {
    pub fn new() -> Self {
        Self { pending: Vec::new() }
    }
}

impl Default for LineFloatParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeParser for LineFloatParser {
    fn feed(&mut self, data: &[u8]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line[..pos]);
            let ch: Vec<f32> = s
                .split(|c| c == ',' || c == ' ' || c == '\t' || c == '$' || c == '#')
                .filter(|t| !t.trim().is_empty())
                .filter_map(|t| t.trim().parse::<f32>().ok())
                .collect();
            if !ch.is_empty() {
                out.push(ch);
            }
        }
        out
    }
}

/// 按协议名构造解析器。
pub fn make_parser(protocol: &str, channels: usize) -> Box<dyn ScopeParser> {
    if protocol == "csv" {
        Box::new(LineFloatParser::new())
    } else {
        Box::new(JustFloatParser::new(channels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn justfloat_roundtrip() {
        let mut p = JustFloatParser::new(2);
        let mut stream = Vec::new();
        let expect: Vec<Vec<f32>> = vec![vec![1.0, -2.5], vec![3.25, 100.0], vec![-7.0, 0.5]];
        for f in &expect {
            stream.extend_from_slice(&f[0].to_le_bytes());
            stream.extend_from_slice(&f[1].to_le_bytes());
            stream.extend_from_slice(&JUSTFLOAT_TAIL);
        }
        stream.extend_from_slice(b"garbage"); // 尾部垃圾不影响已解析帧
        // 分两半喂入(跨批)
        let (a, b) = stream.split_at(stream.len() / 2);
        let mut got = p.feed(a);
        got.extend(p.feed(b));
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], vec![1.0, -2.5]);
        assert_eq!(got[2], vec![-7.0, 0.5]);
    }

    #[test]
    fn justfloat_resync_after_garbage() {
        let mut p = JustFloatParser::new(1);
        let mut d = vec![0xFFu8; 17]; // 未对齐垃圾
        d.extend_from_slice(&5.0f32.to_le_bytes());
        d.extend_from_slice(&JUSTFLOAT_TAIL);
        let got = p.feed(&d);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], vec![5.0]);
    }

    #[test]
    fn csv_lines() {
        let mut p = LineFloatParser::new();
        let got = p.feed(b"1.5,2.5,3\n4,5\npartial");
        assert_eq!(got, vec![vec![1.5, 2.5, 3.0], vec![4.0, 5.0]]);
        let got2 = p.feed(b",6.25\n");
        assert_eq!(got2, vec![vec![6.25]]); // partial + ",6.25" 拼出可解析行(空段被过滤)
    }
}
