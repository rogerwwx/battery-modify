use std::fs;

use crate::vcurve::{Curve, DEFAULT_CURVE};

pub struct Config {
    pub enable_monitor: bool,
    pub enable_temp_comp: bool,
    /// sysfs 轮询间隔(秒)；电量下发固定 30s 一次，不受此值影响
    pub poll_secs: u64,
    pub v_curve: Curve,
    /// 电池内阻(mΩ)，放电时做负载补偿
    pub r_mohm: f64,
    pub min_percent: i64,
    /// 充电时电压兜底路径的封顶百分比
    pub charge_v_cap: f64,
    /// 拔线后弛豫窗口时长(秒)
    pub relax_secs: u64,
    /// 内核电量无变化超时(秒)
    pub stuck_timeout_secs: u64,
    /// 各方向限速：每变化 1% 所需秒数
    pub rate_dis_down: u64,
    pub rate_dis_up: u64,
    pub rate_charge: u64,
    pub rate_charge_stuck: u64,
    pub rate_valve: u64,
    /// 安全阀触发电压（裸端电压 mV）
    pub valve_mv: i64,
    /// 补偿后电压低于该值时 target 封顶
    pub valve_comp_mv: i64,
    pub valve_cap: f64,
    /// 电流符号：0=自动, 1=正为充电, -1=正为放电
    pub current_sign: i32,
    pub calib_log: bool,
}

fn get_val(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn get_bool(content: &str, key: &str, default: bool) -> bool {
    match get_val(content, key) {
        Some(v) => {
            let v = v.to_lowercase();
            v == "true" || v == "1" || v == "yes"
        }
        None => default,
    }
}

fn get_i64(content: &str, key: &str, default: i64) -> i64 {
    get_val(content, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn get_u64(content: &str, key: &str, default: u64) -> u64 {
    get_val(content, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn get_f64(content: &str, key: &str, default: f64) -> f64 {
    get_val(content, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl Config {
    pub fn load(path: &str) -> Config {
        let content = fs::read_to_string(path).unwrap_or_default();
        let v_curve = get_val(&content, "V_CURVE")
            .and_then(|s| Curve::parse(&s))
            .unwrap_or_else(|| Curve::parse(DEFAULT_CURVE).expect("默认曲线解析失败"));
        Config {
            enable_monitor: get_bool(&content, "ENABLE_MONITOR", true),
            enable_temp_comp: get_bool(&content, "ENABLE_TEMP_COMP", true),
            poll_secs: get_u64(&content, "POLL_SECS", 10).clamp(2, 30),
            v_curve,
            r_mohm: get_f64(&content, "R_MOHM", 40.0),
            min_percent: get_i64(&content, "MIN_PERCENT", 1).clamp(0, 10),
            charge_v_cap: get_f64(&content, "CHARGE_V_CAP", 96.0).clamp(50.0, 100.0),
            relax_secs: get_u64(&content, "RELAX_AFTER_UNPLUG_SECS", 300),
            stuck_timeout_secs: get_u64(&content, "KERNEL_STUCK_TIMEOUT_SECS", 900),
            rate_dis_down: get_u64(&content, "RATE_DISCHARGE_DOWN_SECS", 60).max(3),
            rate_dis_up: get_u64(&content, "RATE_DISCHARGE_UP_SECS", 180).max(3),
            rate_charge: get_u64(&content, "RATE_CHARGE_UP_SECS", 45).max(3),
            rate_charge_stuck: get_u64(&content, "RATE_CHARGE_STUCK_SECS", 300).max(3),
            rate_valve: get_u64(&content, "RATE_VALVE_SECS", 10).max(1),
            valve_mv: get_i64(&content, "SHUTDOWN_VALVE_MV", 3150),
            valve_comp_mv: get_i64(&content, "VALVE_COMP_MV", 3250),
            valve_cap: get_f64(&content, "VALVE_CAP_PERCENT", 5.0),
            current_sign: get_i64(&content, "CURRENT_SIGN", 0).clamp(-1, 1) as i32,
            calib_log: get_bool(&content, "CALIB_LOG", false),
        }
    }
}
