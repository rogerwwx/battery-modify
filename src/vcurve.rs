/// 电压(mV) → 电量(%) 分段线性表。
/// 查表输入为负载补偿后的电压，表形应接近电池开路电压曲线。
pub struct Curve {
    points: Vec<(i64, f64)>, // (mV, %)，按 mV 升序
}

/// 默认曲线：3.776~3.933V 区段按实机放电日志拟合（内核电量正常时的 (补偿电压, RM/FCC) 对），
/// 3.776V 以下暂按典型锂电膝部外推，可用 CALIB_LOG 打点补全；顶部以 4.45V 满充为锚
pub const DEFAULT_CURVE: &str = "3050:0,3200:1,3350:3,3450:6,3550:11,3600:15,3650:20,3700:26,3750:32,3776:34,3800:36,3843:40,3872:45,3904:50,3933:55,3960:60,4000:66,4040:73,4090:80,4150:86,4200:90,4250:94,4300:96,4350:98,4400:99,4450:100";

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
