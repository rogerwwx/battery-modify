/// 电压(mV) → 电量(%) 分段线性表。
/// 查表输入为负载补偿后的电压，表形应接近电池开路电压曲线。
pub struct Curve {
    points: Vec<(i64, f64)>, // (mV, %)，按 mV 升序
}

/// 默认曲线：五份实机放电日志（log2/log2t/log3/log4/log4e，2134 个打点，以 RM/FCC 为真值）经
/// 10mV 分箱中位数 → PAVA 加权保序回归 → 偏差≤0.35% 贪心稀疏化（45 点）。
/// 3140mV 起全段有实测覆盖（仅 3330~3380mV 小空隙由拟合桥接）；更低短尾沿 3050:0 外推，
/// 4465mV 为满电锚点。
/// 拟合 RMSE≈0.97%，已接近同电量电压波动 σ≈6.6mV 决定的理论极限(±1%)。
/// 重新拟合用 analysis/fit_curve.py，勿再手工特调。
pub const DEFAULT_CURVE: &str = "3050:0,3160:2.1,3270:5.6,3460:11,3620:17.8,3630:19,3660:19.8,3690:22.4,3710:25.9,3720:29.4,3730:30.6,3740:31,3750:32.6,3800:37.7,3810:39.1,3830:40.5,3850:43,3870:44.5,3900:49.5,3910:52.2,3920:53,3930:55,3960:57.2,3970:59.3,4020:63.6,4070:66.3,4080:67.5,4090:67.6,4100:69.5,4130:71.9,4140:72.2,4150:74,4200:78,4210:79.4,4250:81.2,4260:82.7,4270:82.9,4280:84,4290:86,4310:88.4,4350:90.8,4370:94.1,4410:97.3,4460:97.9,4465:100";

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
