//! 统计工具。

/// 百分位数( samples 原地排序取 p95;空样本返回 0.0)。
pub fn p95(v: &mut Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * 0.95) as usize).saturating_sub(1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p95_basic() {
        let mut v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(p95(&mut v), 95.0);
        assert_eq!(p95(&mut Vec::new()), 0.0);
        let mut one = vec![3.5];
        assert_eq!(p95(&mut one), 3.5);
    }
}
