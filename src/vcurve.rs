/// 电压(mV) → 电量(%) 分段线性表。
/// 查表输入为负载补偿后的电压，表形应接近电池开路电压曲线。
pub struct Curve {
    points: Vec<(i64, f64)>, // (mV, %)，按 mV 升序
}

/// 默认曲线：按 3.00V~4.45V 体系锂电池典型 OCV 给出，
/// 中段锚点已按实机校准（补偿后 3.933V ≈ 55%），可用 CALIB_LOG 打点后进一步拟合替换
pub const DEFAULT_CURVE: &str = "3050:0,3180:1,3350:3,3470:5,3620:10,3700:15,3760:20,3810:30,3855:40,3910:50,3960:60,4000:68,4040:75,4090:82,4150:88,4200:92,4250:95,4300:97,4350:98,4400:99,4450:100";

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
