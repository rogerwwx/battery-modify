/// 电压(mV) → 电量(%) 分段线性表。
/// 查表输入为负载补偿后的电压，表形应接近电池开路电压曲线。
pub struct Curve {
    points: Vec<(i64, f64)>, // (mV, %)，按 mV 升序
}

/// 默认曲线：按 3.00V~4.45V 体系的锂电池典型 OCV 给出，可用 CALIB_LOG 打点后拟合替换
pub const DEFAULT_CURVE: &str = "3050:0,3200:1,3350:3,3450:5,3550:9,3650:15,3750:24,3800:30,3850:36,3900:43,3950:50,4000:57,4050:63,4100:70,4150:76,4200:82,4250:87,4300:91,4350:95,4400:98,4450:100";

impl Curve {
    /// 解析 "mV:percent,mV:percent,..." 格式，mV 必须严格递增且至少 2 个点
    pub fn parse(s: &str) -> Option<Curve> {
        let mut points: Vec<(i64, f64)> = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (mv, pct) = part.split_once(':')?;
            let mv: i64 = mv.trim().parse().ok()?;
            let pct: f64 = pct.trim().parse().ok()?;
            points.push((mv, pct));
        }
        if points.len() < 2 {
            return None;
        }
        points.sort_by(|a, b| a.0.cmp(&b.0));
        for w in points.windows(2) {
            if w[0].0 >= w[1].0 {
                return None;
            }
        }
        Some(Curve { points })
    }

    /// 查表插值，区间外取端点值
    pub fn percent(&self, mv: i64) -> f64 {
        let pts = &self.points;
        let last = pts.len() - 1;
        if mv <= pts[0].0 {
            return pts[0].1;
        }
        if mv >= pts[last].0 {
            return pts[last].1;
        }
        for w in pts.windows(2) {
            if mv >= w[0].0 && mv <= w[1].0 {
                let t = (mv - w[0].0) as f64 / (w[1].0 - w[0].0) as f64;
                return w[0].1 + t * (w[1].1 - w[0].1);
            }
        }
        pts[last].1
    }
}
