use std::fs;

use crate::util::get_current_unix_ts;

pub const STATE_FILE: &str = "/data/adb/battery_smooth.state";
/// 状态文件超过该时长视为过期
const STATE_MAX_AGE_SECS: u64 = 30 * 60;
/// 状态文件值与系统显示值偏差超过该值时放弃续跑，防止跨重启跳变
const STATE_MAX_DEV: f64 = 15.0;

pub struct Smoother {
    pub smooth: f64,
}

/// 初始化顺序：状态文件续跑 > 系统当前显示值 > 内核电量兜底。
/// 不直接用计算值初始化，保证 daemon 启动/重启时显示不跳变。
pub fn init(k_pct_now: Option<f64>, displayed: Option<i64>) -> (Smoother, &'static str) {
    if let Ok(s) = fs::read_to_string(STATE_FILE) {
        let mut smooth: Option<f64> = None;
        let mut ts: u64 = 0;
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("smooth=") {
                smooth = v.trim().parse().ok();
            }
            if let Some(v) = line.strip_prefix("ts=") {
                ts = v.trim().parse().unwrap_or(0);
            }
        }
        if let Some(v) = smooth {
            if (1.0..=100.0).contains(&v)
                && get_current_unix_ts().saturating_sub(ts) <= STATE_MAX_AGE_SECS
            {
                let dev_ok = match displayed {
                    Some(d) => (v - d as f64).abs() <= STATE_MAX_DEV,
                    None => true,
                };
                if dev_ok {
                    return (Smoother { smooth: v }, "状态文件续跑");
                }
            }
        }
    }
    if let Some(d) = displayed {
        return (Smoother { smooth: d as f64 }, "系统显示值");
    }
    (
        Smoother {
            smooth: k_pct_now.unwrap_or(50.0).clamp(1.0, 100.0),
        },
        "内核电量兜底",
    )
}

pub fn save(smooth: f64, status: &str) {
    let _ = fs::write(
        STATE_FILE,
        format!(
            "smooth={:.2}\nts={}\nstatus={}\n",
            smooth,
            get_current_unix_ts(),
            status
        ),
    );
}
