use std::thread::sleep;
use std::time::Duration;

use crate::util::{read_sys_file, read_sys_file_i64, write_log};

pub const BATTERY_PATH: &str = "/sys/class/power_supply/battery";
/// 节点已按实机确认，直接写死
const V_PATH: &str = "/sys/class/power_supply/battery/voltage_now";
const I_PATH: &str = "/sys/class/power_supply/battery/current_now";
const RM_PATH: &str = "/sys/class/power_supply/battery/charge_counter";
const FCC_PATH: &str = "/sys/class/power_supply/battery/charge_full";

pub struct Source {
    /// 符号修正后电流：正值 = 充电
    pub sign: i32,
}

pub struct Reading {
    pub v_mv: i64,
    /// 符号修正后的电流(mA)，正 = 充电
    pub i_ma: Option<f64>,
    pub status: String,
    pub rm_mah: Option<f64>,
    pub fcc_mah: Option<f64>,
}

pub fn probe(current_sign_cfg: i32) -> Source {
    let (sign, sign_src) = if current_sign_cfg != 0 {
        (current_sign_cfg, "配置指定")
    } else {
        (detect_sign(), "自动检测")
    };
    write_log(&format!(
        "电流符号={} ({}) | 节点: {} / {} / {} / {}",
        sign, sign_src, V_PATH, I_PATH, RM_PATH, FCC_PATH
    ));
    Source { sign }
}

/// 依据“放电时电流应为负”自动检测符号；无法判定时按高通惯例（正 = 充电）
fn detect_sign() -> i32 {
    let mut dis_sum: i64 = 0;
    let mut dis_n = 0;
    let mut chg_sum: i64 = 0;
    let mut chg_n = 0;
    for _ in 0..5 {
        let status = read_sys_file(&format!("{}/status", BATTERY_PATH));
        let raw = read_sys_file_i64(I_PATH);
        if status == "Discharging" {
            dis_sum += raw;
            dis_n += 1;
        } else if status == "Charging" {
            chg_sum += raw;
            chg_n += 1;
        }
        sleep(Duration::from_millis(300));
    }
    if dis_n > 0 {
        if dis_sum > 0 {
            -1
        } else {
            1
        }
    } else if chg_n > 0 && chg_sum < 0 {
        -1
    } else {
        1
    }
}

/// µAh/mAh 混用判别：手机电池的 mAh 数远小于 10 万
fn norm_mah(v: i64) -> f64 {
    let mut v = v as f64;
    if v > 100_000.0 {
        v /= 1000.0;
    }
    v
}

impl Source {
    pub fn read(&self) -> Reading {
        let v_raw = read_sys_file_i64(V_PATH);
        // voltage_now 以 µV 为主，个别机型以 mV 上报
        let v_mv = if v_raw > 100_000 { v_raw / 1000 } else { v_raw };
        let status = read_sys_file(&format!("{}/status", BATTERY_PATH));
        let i_ma = {
            let raw = read_sys_file_i64(I_PATH) as f64;
            if raw != 0.0 {
                // 电流以 µA 为主；|值| < 1000 时视为 mA 上报的机型
                let ma = if raw.abs() >= 1000.0 { raw / 1000.0 } else { raw };
                Some(ma * self.sign as f64)
            } else {
                None
            }
        };
        let mut rm = norm_mah(read_sys_file_i64(RM_PATH));
        let fcc = norm_mah(read_sys_file_i64(FCC_PATH));
        if fcc > 0.0 && rm > fcc * 1.5 {
            rm /= 1000.0; // RM/FCC 单位不一致兜底
        }
        let rm_mah = if rm > 0.0 { Some(rm) } else { None };
        let fcc_mah = if fcc > 0.0 { Some(fcc) } else { None };
        Reading {
            v_mv,
            i_ma,
            status,
            rm_mah,
            fcc_mah,
        }
    }
}
