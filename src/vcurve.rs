/// 电压(mV) → 电量(%) 分段线性表。
/// 查表输入为负载补偿后的电压，表形应接近电池开路电压曲线。
pub struct Curve {
    points: Vec<(i64, f64)>, // (mV, %)，按 mV 升序
}

/// 默认曲线：全段按实机放电日志拟合（内核电量正常时的 (补偿电压, RM/FCC) 对）：
/// 低段 3.38~3.93V、顶段 4.39~4.47V 来自实测，仅 3.38V 以下短尾为外推；
/// 4.0~4.39V 肩部按两端锚点与斜率渐变插值
pub const DEFAULT_CURVE: &str = "3050:0,3150:3,3250:5,3350:7.5,3400:9,3430:10,3451:11,3475:11.5,3489:12.5,3513:13.5,3540:14.5,3580:16,3605:17.5,3620:18,3640:19,3655:20,3670:21,3680:22,3690:23,3700:24,3710:25,3740:29,3776:34,3800:36,3843:40,3872:45,3904:50,3933:55,3960:60,4000:66,4040:73,4090:80,4150:84,4200:87,4250:90,4300:92.5,4350:94,4386:96,4430:98.5,4465:100";

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
