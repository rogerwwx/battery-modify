/// 滑动窗口中位数（去单点毛刺）+ EMA（平滑）
pub struct MedianEma {
    window: usize,
    alpha: f64,
    buf: Vec<f64>,
    ema: Option<f64>,
}

impl MedianEma {
    pub fn new(window: usize, alpha: f64) -> MedianEma {
        MedianEma {
            window,
            alpha,
            buf: Vec::new(),
            ema: None,
        }
    }

    pub fn push(&mut self, v: f64) -> f64 {
        self.buf.push(v);
        if self.buf.len() > self.window {
            self.buf.remove(0);
        }
        let mut sorted = self.buf.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med = sorted[sorted.len() / 2];
        self.ema = Some(match self.ema {
            None => med,
            Some(e) => e + self.alpha * (med - e),
        });
        self.ema.unwrap_or(med)
    }

    /// 读数失败时保持上一输出，避免滤波器被无效值拖走
    pub fn last_or(&self, default: f64) -> f64 {
        self.ema.unwrap_or(default)
    }
}
