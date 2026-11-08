// PortPilot · 频域视图:radix-2 迭代 FFT + Hann 窗 + 幅度谱
// 约束:零第三方依赖;仅处理 2 的幂长度;所有公开函数可单测。
// 参考:标准 Cooley–Tukey 位反转迭代实现。

/// 原位 radix-2 迭代 FFT。`re`/`im` 长度必须为 2 的幂。
fn fft_inplace(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    if n <= 1 {
        return;
    }
    // 位反转重排(usize 为 64 位,须按全宽反转再右移)
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = (i.reverse_bits() >> (usize::BITS - bits)) as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // 蝶形迭代
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let (w_re, w_im) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cur_re, mut cur_im) = (1.0, 0.0);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let t_re = re[b] * cur_re - im[b] * cur_im;
                let t_im = re[b] * cur_im + im[b] * cur_re;
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next_re = cur_re * w_re - cur_im * w_im;
                cur_im = cur_re * w_im + cur_im * w_re;
                cur_re = next_re;
            }
        }
        len <<= 1;
    }
}

/// Hann 窗(周期式):w[i] = 0.5 - 0.5·cos(2πi/N)。
pub fn hann_window(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
        .collect()
}

/// 频谱结果:幅度谱(N/2+1 个 bin,含直流与奈奎斯特)与峰值信息。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Spectrum {
    /// 各 bin 幅度(线性);bin i 对应频率 i·bin_hz
    pub mag: Vec<f64>,
    /// 峰值 bin 序号(跳过直流 bin 0)
    pub peak_bin: usize,
    pub peak_hz: f64,
    pub peak_mag: f64,
}

/// 对实信号做 Hann 窗 FFT,返回单边幅度谱。
/// `fs_hz`:采样率;`out_len` 只取 2 的幂(如 512/1024/2048),不足补零、过长取末尾。
/// 返回的 mag 已做 Hann 相干增益(2.0)归一,幅度与原始时域峰值同量纲。
pub fn spectrum(samples: &[f32], fs_hz: f64, out_len: usize) -> Spectrum {
    assert!(out_len.is_power_of_two() && out_len >= 2, "out_len 必须为 2 的幂");
    let n = out_len;
    // 取末尾 n 个样本(不足补零)
    let mut re = vec![0.0f64; n];
    if !samples.is_empty() {
        let take = n.min(samples.len());
        for (i, s) in samples[samples.len() - take..].iter().enumerate() {
            re[n - take + i] = *s as f64;
        }
    }
    let win = hann_window(n);
    for i in 0..n {
        re[i] *= win[i];
    }
    let mut im = vec![0.0f64; n];
    fft_inplace(&mut re, &mut im);

    let half = n / 2;
    // 幅度归一:|X[k]|·2/Σw 恢复原始时域幅度(DC/奈奎斯特不成对,不乘 2)。
    // Hann 的 Σw = N/2,故等效 |X|·4/N;对准 bin 正弦结果恰为幅值 A。
    let win_sum: f64 = win.iter().sum();
    let mut mag = Vec::with_capacity(half + 1);
    for k in 0..=half {
        let scale = if k == 0 || k == half { 1.0 } else { 2.0 };
        let m = (re[k] * re[k] + im[k] * im[k]).sqrt() * scale / win_sum;
        mag.push(m);
    }
    // 峰值:跳过直流
    let mut peak_bin = 1;
    let mut peak_mag = 0.0;
    for (k, &m) in mag.iter().enumerate().skip(1) {
        if m > peak_mag {
            peak_mag = m;
            peak_bin = k;
        }
    }
    let bin_hz = if fs_hz > 0.0 { fs_hz / n as f64 } else { 0.0 };
    Spectrum { mag, peak_bin, peak_hz: peak_bin as f64 * bin_hz, peak_mag }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_sine(n: usize, fs: f64, f: f64, amp: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * f * i as f64 / fs).sin()) as f32)
            .collect()
    }

    #[test]
    fn sine_peaks_at_expected_bin() {
        // 1000Hz 正弦 @ 8kHz 采样,1024 点 → bin = 1000/(8000/1024) = 128
        let n = 1024;
        let s = gen_sine(n, 8000.0, 1000.0, 1.0);
        let sp = spectrum(&s, 8000.0, n);
        assert_eq!(sp.peak_bin, 128, "峰值应落在 bin 128");
        assert!((sp.peak_hz - 1000.0).abs() < 8.0, "峰值频率≈1000Hz,得 {}", sp.peak_hz);
        // 幅度应在 1.0±10%(Hann 频谱泄漏导致轻微展宽)
        assert!((sp.peak_mag - 1.0).abs() < 0.1, "峰值幅度≈1.0,得 {}", sp.peak_mag);
        // 单边谱长度 = N/2+1
        assert_eq!(sp.mag.len(), n / 2 + 1);
    }

    #[test]
    fn dc_goes_to_bin0_and_peak_skips_it() {
        // 直流 1.0 + 幅值 2.0 正弦(bin 32):峰值应跳过 bin 0 选中正弦;
        // 且 mag[0] 应恢复直流幅值 1.0(Hann 泄漏 bin1 ≈ 0.5 < 2.0)
        let n = 256;
        let mut s: Vec<f32> = vec![1.0; n];
        for (i, v) in s.iter_mut().enumerate() {
            *v += (2.0 * std::f64::consts::PI * 32.0 * i as f64 / n as f64).sin() as f32 * 2.0;
        }
        let sp = spectrum(&s, 256.0, n);
        assert!(sp.peak_bin >= 1, "峰值不应是直流 bin");
        assert_eq!(sp.peak_bin, 32);
        assert!((sp.mag[0] - 1.0).abs() < 0.01, "DC 幅度应≈1.0,得 {}", sp.mag[0]);
    }

    #[test]
    fn parseval_energy_conserved() {
        // Parseval(离散双边): Σ|X[k]|² = N·Σ|x[n]·w[n]|²
        // spectrum 的反解:|X[k]| = mag[k]·Σw / scale_k(scale: DC/奈奎斯特=1,其余=2)
        let n = 512;
        let s = gen_sine(n, 1024.0, 64.0, 2.0);
        let win = hann_window(n);
        let win_sum: f64 = win.iter().sum();
        let t_energy: f64 = s
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let x = v as f64 * win[i];
                x * x
            })
            .sum();
        let sp = spectrum(&s, 1024.0, n);
        let half = sp.mag.len() - 1;
        // 单边谱重建双边能量:|X|² 总和 = x2[0] + x2[Nyq] + 2·Σ(中间 bin)
        let x2: Vec<f64> = sp
            .mag
            .iter()
            .enumerate()
            .map(|(k, &m)| {
                let scale = if k == 0 || k == half { 1.0 } else { 2.0 };
                (m * win_sum / scale).powi(2)
            })
            .collect();
        let f_energy = x2[0] + x2[half] + 2.0 * x2[1..half].iter().sum::<f64>();
        let expected = (n as f64) * t_energy;
        assert!(
            (f_energy - expected).abs() / expected < 0.02,
            "Parseval 失败: 频域 {} vs 时域 {}",
            f_energy,
            expected
        );
    }

    #[test]
    fn zero_input_gives_flat_zero() {
        let n = 128;
        let s = vec![0.0f32; n];
        let sp = spectrum(&s, 1000.0, n);
        assert!(sp.peak_mag < 1e-9, "全零输入峰值应≈0,得 {}", sp.peak_mag);
        assert_eq!(sp.peak_bin, 1); // 跳过直流后的第一个 bin
    }

    #[test]
    fn tail_extraction_and_zero_padding() {
        // 输入长于 out_len → 取末尾;输入短于 → 前面补零
        let fs = 1024.0;
        let long = gen_sine(4096, fs, 128.0, 1.0); // bin = 128/(1024/1024)=128
        let sp1 = spectrum(&long, fs, 1024);
        assert_eq!(sp1.peak_bin, 128);
        let short = gen_sine(300, fs, 102.4, 1.0); // 300 点,补零到 1024
        let sp2 = spectrum(&short, fs, 1024);
        // 补零后分辨率 1Hz,峰应在 102Hz 附近(bin 102±2)
        assert!(
            sp2.peak_hz > 99.0 && sp2.peak_hz < 106.0,
            "补零峰值≈102Hz,得 {}",
            sp2.peak_hz
        );
    }
}

