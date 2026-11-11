// PortPilot · 文本流自动结构识别(SerialWeb 点子自研实现)
// 场景:下位机 printf 出的文本流(裸浮点 / key=value / CSV),用户不想手配模板。
// 输入最近 N 行,输出稳定结构的识别结果与建议;全部纯函数可单测。

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SniffKind {
    /// 每行 k 个浮点,分隔符 delim —— 可直接配示波器 CSV
    CsvFloats { k: usize, delim: String },
    /// key=value 对(逗号/空格分隔)—— 建议字段名列表
    KeyValue { fields: Vec<String> },
    /// 每行单个浮点
    SingleFloat,
    /// 未能识别稳定结构
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct SniffReport {
    pub kind: SniffKind,
    /// 参与判定的行占比(0~1);≥0.8 视为稳定
    pub confidence: f64,
    /// 建议示波器通道数(CsvFloats/SingleFloat 有值)
    pub suggest_channels: Option<usize>,
    /// 样例行(最多 3 条,截断展示)
    pub samples: Vec<String>,
}

/// 单行分词:按 delim 或 [,;\t ] 混合切分,返回片段列表
fn tokenize(line: &str, delim: Option<char>) -> Vec<String> {
    let t = line.trim();
    if t.is_empty() {
        return vec![];
    }
    match delim {
        Some(d) => t.split(d).map(|s| s.trim().to_string()).collect(),
        None => t.split(|c: char| c == ',' || c == ';' || c == '\t' || c == ' ')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
    }
}

fn is_f32(s: &str) -> bool {
    s.parse::<f32>().is_ok()
}

/// key=value 形态:`a=1.2` / `temp: 36.5`(冒号也算)
fn parse_kv(line: &str) -> Option<Vec<(String, f64)>> {
    let t = line.trim();
    if t.is_empty() {
        return None;
    }
    // 分节策略:有 '='(如 a=1 b=2)按空白切;冒号形态(dist: 1.2 / a: 1, b: 2)
    // 值与键之间有空格,空白切会把 k/v 拆散 —— 按逗号/分号切,节内找冒号
    let segments: Vec<&str> = if t.contains('=') {
        t.split(|c: char| c == ',' || c == ';' || c == ' ').collect()
    } else {
        t.split(|c: char| c == ',' || c == ';').collect()
    };
    let mut out = Vec::new();
    for seg in segments {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let (k, v) = if let Some(p) = seg.split_once('=') {
            (p.0.trim(), p.1.trim())
        } else if let Some(p) = seg.split_once(':') {
            (p.0.trim(), p.1.trim())
        } else {
            return None;
        };
        if k.is_empty() || !is_f32(v) {
            return None;
        }
        out.push((k.to_string(), v.parse().unwrap()));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 主入口:对最近 N 行做结构识别。
/// 策略:先试 key=value(更specific),再试 CSV 浮点(要求列数一致),
/// 取占比最高且 ≥0.5 的结构;不足则 Unknown。
pub fn sniff(lines: &[String]) -> SniffReport {
    let valid: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if valid.is_empty() {
        return SniffReport {
            kind: SniffKind::Unknown,
            confidence: 0.0,
            suggest_channels: None,
            samples: vec![],
        };
    }
    let total = valid.len();

    // --- 候选 1:key=value ---
    let mut kv_fields: Option<Vec<String>> = None;
    let mut kv_hits = 0usize;
    for l in &valid {
        if let Some(pairs) = parse_kv(l) {
            kv_hits += 1;
            let names: Vec<String> = pairs.into_iter().map(|(k, _)| k).collect();
            match &kv_fields {
                None => kv_fields = Some(names),
                Some(prev) => {
                    // 字段名集与顺序一致才算同构
                    if *prev != names {
                        // 宽容:数量一致即视为同构(名序漂移常见于 printf)
                        if prev.len() != names.len() {
                            kv_fields = None;
                        }
                    }
                }
            }
        }
    }
    if let Some(fields) = &kv_fields {
        let conf = kv_hits as f64 / total as f64;
        if conf >= 0.5 {
            return SniffReport {
                kind: SniffKind::KeyValue { fields: fields.clone() },
                confidence: conf,
                suggest_channels: Some(fields.len()),
                samples: valid.iter().take(3).map(|s| (*s).clone()).collect(),
            };
        }
    }

    // --- 候选 2:CSV 浮点(无分隔参数,自动探测每行主分隔符) ---
    // 探测:取首行含 ',' 用 ',';含 ';' 用 ';';含 '\t' 用 '\t';否则空白
    let delim = if valid[0].contains(',') {
        Some(',')
    } else if valid[0].contains(';') {
        Some(';')
    } else if valid[0].contains('\t') {
        Some('\t')
    } else {
        None
    };
    let mut col_counts: std::collections::BTreeMap<usize, usize> = Default::default();
    let mut float_hits = 0usize;
    for l in &valid {
        let toks = tokenize(l, delim);
        if toks.is_empty() {
            continue;
        }
        if toks.iter().all(|t| is_f32(t)) {
            float_hits += 1;
            *col_counts.entry(toks.len()).or_insert(0) += 1;
        }
    }
    if let Some((&k, &cnt)) = col_counts.iter().max_by_key(|(_, c)| *c) {
        let conf = cnt as f64 / total as f64;
        if conf >= 0.5 && k >= 1 {
            let delim_s = match delim {
                Some(',') => ",".into(),
                Some(';') => ";".into(),
                Some('\t') => "\\t".into(),
                _ => " ".into(),
            };
            return SniffReport {
                kind: if k == 1 {
                    SniffKind::SingleFloat
                } else {
                    SniffKind::CsvFloats { k, delim: delim_s }
                },
                confidence: conf,
                suggest_channels: Some(k),
                samples: valid.iter().take(3).map(|s| (*s).clone()).collect(),
            };
        }
    }

    SniffReport {
        kind: SniffKind::Unknown,
        confidence: float_hits as f64 / total as f64,
        suggest_channels: None,
        samples: valid.iter().take(3).map(|s| (*s).clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn csv_floats_two_columns() {
        let r = sniff(&lines(&["1.0,2.0", "3.5,4.5", "-1.0,0.0", "2.0,2.0"]));
        match r.kind {
            SniffKind::CsvFloats { k, delim } => {
                assert_eq!(k, 2);
                assert_eq!(delim, ",");
            }
            other => panic!("应为 CsvFloats,得 {:?}", other),
        }
        assert_eq!(r.suggest_channels, Some(2));
        assert!(r.confidence >= 0.99);
    }

    #[test]
    fn key_value_with_names() {
        let r = sniff(&lines(&[
            "temp=25.5,hum=60.0",
            "temp=25.6,hum=59.8",
            "temp=25.4,hum=60.2",
        ]));
        match r.kind {
            SniffKind::KeyValue { fields } => assert_eq!(fields, vec!["temp", "hum"]),
            other => panic!("应为 KeyValue,得 {:?}", other),
        }
    }

    #[test]
    fn key_value_colon_separator() {
        let r = sniff(&lines(&["dist: 120.5", "dist: 121.0", "dist: 119.5"]));
        match r.kind {
            SniffKind::KeyValue { fields } => assert_eq!(fields, vec!["dist"]),
            other => panic!("应为 KeyValue,得 {:?}", other),
        }
    }

    #[test]
    fn single_float_lines() {
        let r = sniff(&lines(&["1.5", "2.5", "-3.0", "0.0"]));
        assert!(matches!(r.kind, SniffKind::SingleFloat));
        assert_eq!(r.suggest_channels, Some(1));
    }

    #[test]
    fn mixed_majority_wins() {
        // 3 行 CSV + 1 行杂讯 → 多数胜出
        let r = sniff(&lines(&["1.0,2.0", "3.0,4.0", "5.0,6.0", "hello world"]));
        assert!(matches!(r.kind, SniffKind::CsvFloats { k: 2, .. }));
        assert!((r.confidence - 0.75).abs() < 0.01);
    }

    #[test]
    fn unstable_structure_is_unknown() {
        // 列数漂移 → 不稳定
        let r = sniff(&lines(&["1.0,2.0", "1.0,2.0,3.0", "1.0", "4.0,5.0,6.0,7.0"]));
        assert!(matches!(r.kind, SniffKind::Unknown), "得 {:?}", r.kind);
    }

    #[test]
    fn space_delimited_floats() {
        let r = sniff(&lines(&["1.0 2.0 3.0", "4.0 5.0 6.0", "7.0 8.0 9.0"]));
        match r.kind {
            SniffKind::CsvFloats { k, delim } => {
                assert_eq!(k, 3);
                assert_eq!(delim, " ");
            }
            other => panic!("应为 CsvFloats,得 {:?}", other),
        }
    }

    #[test]
    fn empty_input_is_unknown() {
        let r = sniff(&lines(&["", "  "]));
        assert!(matches!(r.kind, SniffKind::Unknown));
    }
}
