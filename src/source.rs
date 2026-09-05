use std::thread::sleep;
use std::time::Duration;

use crate::util::{read_sys_file, read_sys_file_i64, write_log};

pub const BATTERY_PATH: &str = "/sys/class/power_supply/battery";
const BMS_PATH: &str = "/sys/class/power_supply/bms";
/// 旧版写入的满充峰值缓存，作为 FCC 节点缺失时的兜底
pub const MAX_CHARGE_COUNTER_FILE: &str = "/data/adb/battery_max_charge_counter";

pub struct Sources {
    pub v_path: String,
    pub i_path: Option<String>,
    pub rm_path: Option<String>,
    pub fcc_path: Option<String>,
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

fn first_nonzero(paths: &[&str]) -> Option<String> {
    for p in paths {
        if read_sys_file_i64(p) != 0 {
            return Some((*p).to_string());
        }
    }
    None
}

pub fn probe(current_sign_cfg: i32) -> Sources {
    let v_path = first_nonzero(&[
        &format!("{}/voltage_now", BATTERY_PATH),
        &format!("{}/voltage_now", BMS_PATH),
    ])
    .unwrap_or_else(|| format!("{}/voltage_now", BATTERY_PATH));

    let i_path = first_nonzero(&[
        &format!("{}/current_now", BATTERY_PATH),
        &format!("{}/current_now", BMS_PATH),
    ]);

    let rm_path = first_nonzero(&[
        &format!("{}/charge_counter", BATTERY_PATH),
        &format!("{}/charge_now", BATTERY_PATH),
        &format!("{}/charge_counter", BMS_PATH),
        &format!("{}/charge_now", BMS_PATH),
    ]);

    let fcc_path = first_nonzero(&[
        &format!("{}/charge_full", BATTERY_PATH),
        &format!("{}/charge_full", BMS_PATH),
    ])
    .or_else(|| {
        if read_sys_file_i64(MAX_CHARGE_COUNTER_FILE) > 0 {
            Some(MAX_CHARGE_COUNTER_FILE.to_string())
        } else {
            None
        }
    });

    let (sign, sign_src) = if current_sign_cfg != 0 {
        (current_sign_cfg, "配置指定")
    } else {
        (detect_sign(&i_path), "自动检测")
    };

    write_log(&format!(
        "节点探测: v={} i={:?} rm={:?} fcc={:?} | 电流符号={} ({})",
        v_path, i_path, rm_path, fcc_path, sign, sign_src
    ));

    Sources {
        v_path,
        i_path,
        rm_path,
        fcc_path,
        sign,
    }
}

/// 依据“放电时电流应为负”自动检测符号；无法判定时按高通惯例（正 = 充电）
fn detect_sign(i_path: &Option<String>) -> i32 {
    let path = match i_path {
        Some(p) => p.clone(),
        None => return 1,
    };
    let mut dis_sum: i64 = 0;
    let mut dis_n = 0;
    let mut chg_sum: i64 = 0;
    let mut chg_n = 0;
    for _ in 0..5 {
        let status = read_sys_file(&format!("{}/status", BATTERY_PATH));
        let raw = read_sys_file_i64(&path);
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

impl Sources {
    pub fn read(&self) -> Reading {
        let v_mv = read_sys_file_i64(&self.v_path);
        let status = read_sys_file(&format!("{}/status", BATTERY_PATH));
        let i_ma = self.i_path.as_ref().map(|p| {
            let raw = read_sys_file_i64(p) as f64;
            // 内核电流以 µA 为主；|值| < 1000 时视为 mA 上报的机型
            let ma = if raw.abs() >= 1000.0 { raw / 1000.0 } else { raw };
            ma * self.sign as f64
        });
        let (rm_mah, fcc_mah) = match (&self.rm_path, &self.fcc_path) {
            (Some(rm_p), Some(fcc_p)) => {
                let mut rm = norm_mah(read_sys_file_i64(rm_p));
                let fcc = norm_mah(read_sys_file_i64(fcc_p));
                if fcc > 0.0 {
                    if rm > fcc * 1.5 {
                        rm /= 1000.0; // RM/FCC 单位不一致兜底
                    }
                    if rm > 0.0 {
                        (Some(rm), Some(fcc))
                    } else {
                        (None, Some(fcc))
                    }
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        };
        Reading {
            v_mv,
            i_ma,
            status,
            rm_mah,
            fcc_mah,
        }
    }
}
