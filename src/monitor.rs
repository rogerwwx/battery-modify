use std::process::Command;
use std::time::Duration;

use nix::sys::timerfd::{ClockId, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};

use crate::config::Config;
use crate::filter::MedianEma;
use crate::smoother;
use crate::source::{self, Reading};
use crate::util::{get_current_unix_ts, write_log};

/// 电量下发周期：与原版一致固定 30s，不随轮询间隔变化
const PUBLISH_SECS: u64 = 30;
/// 充放电状态切换防抖时长
const DEBOUNCE_SECS: u64 = 9;
/// 安全阀确认时长（端电压持续低于阈值）
const VALVE_CONFIRM_SECS: u64 = 30;
/// 电压 EMA 时间常数
const V_EMA_TAU_SECS: f64 = 60.0;
const V_MEDIAN_WINDOW: usize = 5;
/// 内核电量单拍跳变超过该值视为毛刺，需下一拍确认才接受
const KERNEL_GLITCH_JUMP: f64 = 8.0;
/// 毛刺确认：与暂存值相差 ≤3 才接受
const KERNEL_GLITCH_CONFIRM: f64 = 3.0;
/// 放电时内核电量高出电压百分比超过该值则不再完全信任内核
const KERNEL_TRUST_GAP: f64 = 20.0;
/// 安全阀解除回差(mV)
const VALVE_HYST_MV: i64 = 150;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Unknown,
    Discharging,
    Charging,
    Other,
}

impl Mode {
    fn from_status(s: &str) -> Mode {
        match s {
            "Discharging" => Mode::Discharging,
            "Charging" => Mode::Charging,
            _ => Mode::Other,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Mode::Unknown => "Unknown",
            Mode::Discharging => "放电",
            Mode::Charging => "充电",
            Mode::Other => "其他",
        }
    }
}

fn kernel_pct(r: &Reading) -> Option<f64> {
    match (r.rm_mah, r.fcc_mah) {
        (Some(rm), Some(fcc)) if rm > 0.0 && fcc > 0.0 => Some((rm * 100.0 / fcc).min(100.0)),
        _ => None,
    }
}

/// 读取系统当前显示电量（仅用于接管初始化，防开机跳变）
fn read_displayed_level() -> Option<i64> {
    let out = Command::new("dumpsys").arg("battery").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("level:") {
            if let Ok(v) = rest.trim().parse::<i64>() {
                return Some(v);
            }
        }
    }
    None
}

pub fn run(cfg: &Config) {
    write_log("电量接管启动：电压模拟 + 内核融合 + 平滑限速");
    let src = source::probe(cfg.current_sign);

    let first = src.read();
    let k0 = kernel_pct(&first);
    let displayed = read_displayed_level();
    let (mut sm, init_src) = smoother::init(k0, displayed);
    write_log(&format!(
        "smooth 初始化 {:.1}% (来源:{}) | 系统显示 {:?} | 内核电量 {:?}",
        sm.smooth, init_src, displayed, k0
    ));

    // 轮询间隔相关：所有周期性逻辑按时间换算，行为与轮询间隔无关
    let poll = cfg.poll_secs;
    let v_alpha = (poll as f64 / V_EMA_TAU_SECS).min(0.5);
    let debounce_ticks = ((DEBOUNCE_SECS + poll - 1) / poll).max(2) as u32;
    let valve_ticks_needed = ((VALVE_CONFIRM_SECS + poll - 1) / poll).max(1) as u32;

    let tfd = TimerFd::new(ClockId::CLOCK_MONOTONIC, TimerFlags::empty())
        .expect("TimerFd create failed");
    tfd.set(
        Expiration::Interval(Duration::from_secs(poll).into()),
        TimerSetTimeFlags::empty(),
    )
    .expect("TimerFd set failed");

    let mut filt = MedianEma::new(V_MEDIAN_WINDOW, v_alpha);

    let mut mode = Mode::Unknown;
    let mut status_str = String::new();
    let mut pending: Option<String> = None;
    let mut pending_count: u32 = 0;

    let mut relax_until: u64 = 0;
    let mut last_k_move: u64 = get_current_unix_ts();
    let mut prev_k: Option<f64> = None;
    let mut k_accepted: Option<f64> = None;
    let mut k_pending: Option<f64> = None;

    let mut valve_count: u32 = 0;
    let mut valve_active = false;

    let mut full_count: u32 = 0;
    let mut full_latched = false;
    let mut post_full_idle = false;

    let mut last_pub_ts: u64 = 0; // 0 = 首个可下发拍立即发布
    let mut force_publish = false;

    loop {
        let _ = tfd.wait().expect("TimerFd wait failed");
        let now_ts = get_current_unix_ts();

        let rd = src.read();
        let v_mv_raw = rd.v_mv;
        let status = rd.status.clone();

        // ---- 模式切换：连续 3 拍一致才切换（防插拔线抖动），首拍直接采纳 ----
        if status == status_str {
            pending = None;
            pending_count = 0;
        } else {
            if pending.as_deref() == Some(status.as_str()) {
                pending_count += 1;
            } else {
                pending = Some(status.clone());
                pending_count = 1;
            }
            if mode == Mode::Unknown || pending_count >= debounce_ticks {
                status_str = status.clone();
                mode = Mode::from_status(&status_str);
                pending = None;
                pending_count = 0;
                match mode {
                    Mode::Discharging => {
                        relax_until = now_ts + cfg.relax_secs;
                        if post_full_idle {
                            // 满电 reset 后重新接管，显示值即内核真实值
                            sm.smooth = k_accepted.unwrap_or(sm.smooth);
                            post_full_idle = false;
                        }
                        force_publish = true;
                        write_log(&format!(
                            "切换→放电 | smooth={:.1} k={:?} v={}mV",
                            sm.smooth, k_accepted, v_mv_raw
                        ));
                    }
                    Mode::Charging => {
                        last_k_move = now_ts;
                        prev_k = None;
                        post_full_idle = false;
                        valve_count = 0;
                        valve_active = false;
                        write_log(&format!(
                            "切换→充电 | smooth={:.1} k={:?}",
                            sm.smooth, k_accepted
                        ));
                    }
                    _ => {
                        valve_count = 0;
                        valve_active = false;
                        write_log(&format!("切换→{} | smooth={:.1}", status_str, sm.smooth));
                    }
                }
            }
        }

        // ---- 低电安全阀（按裸端电压判定，回差解除）----
        if mode == Mode::Discharging && v_mv_raw > 0 {
            if v_mv_raw < cfg.valve_mv {
                valve_count += 1;
                if valve_count >= valve_ticks_needed && !valve_active {
                    valve_active = true;
                    write_log(&format!("安全阀触发：端电压 {}mV，快速下探", v_mv_raw));
                }
            } else if v_mv_raw > cfg.valve_mv + VALVE_HYST_MV {
                valve_count = 0;
                if valve_active {
                    valve_active = false;
                    write_log("安全阀解除");
                }
            }
        } else {
            valve_count = 0;
            valve_active = false;
        }

        // ---- 电压：负载补偿 → 查表 → 中位数 → EMA ----
        let v_comp_mv = match rd.i_ma {
            Some(i) if mode == Mode::Discharging && i < 0.0 => {
                v_mv_raw + ((-i) * cfg.r_mohm / 1000.0) as i64
            }
            _ => v_mv_raw,
        };
        let v_pct_raw = cfg
            .v_curve
            .percent(v_comp_mv)
            .clamp(cfg.min_percent as f64, 100.0);
        let v_pct = if v_mv_raw > 0 {
            filt.push(v_pct_raw)
        } else {
            filt.last_or(50.0) // 读不到电压时保持原值
        };

        // ---- 内核电量 RM*100/FCC，带毛刺拒绝 ----
        let k_now = kernel_pct(&rd);
        let mut k_pct = k_accepted;
        if let Some(v) = k_now {
            match k_accepted {
                None => {
                    k_accepted = Some(v);
                    k_pct = Some(v);
                }
                Some(last) => {
                    if (v - last).abs() <= KERNEL_GLITCH_JUMP {
                        k_pending = None;
                        k_accepted = Some(v);
                        k_pct = Some(v);
                    } else if let Some(p) = k_pending {
                        if (v - p).abs() <= KERNEL_GLITCH_CONFIRM {
                            k_pending = None;
                            k_accepted = Some(v);
                            k_pct = Some(v);
                            write_log(&format!("内核电量跳变确认: {:.0}% → {:.0}%", last, v));
                        } else {
                            k_pending = Some(v);
                        }
                    } else {
                        k_pending = Some(v);
                    }
                }
            }
        }

        // ---- 内核电量活跃度跟踪（所有模式通用，卡死检测）----
        if let Some(k) = k_pct {
            if prev_k.map_or(true, |p| (k - p).abs() >= 1.0) {
                last_k_move = now_ts;
            }
            prev_k = Some(k);
        }
        let stuck = now_ts.saturating_sub(last_k_move) > cfg.stuck_timeout_secs;

        let in_relax = mode == Mode::Discharging && now_ts < relax_until;
        let boost = mode == Mode::Discharging && v_pct < sm.smooth - 10.0;

        // ---- 目标融合 ----
        let mut target = if valve_active && mode == Mode::Discharging {
            cfg.min_percent as f64
        } else {
            match mode {
                Mode::Discharging => match k_pct {
                    // 内核长期不动（卡死）或远高于电压时不再完全采信，回到纯电压
                    Some(k) if !stuck && k - v_pct <= KERNEL_TRUST_GAP => k.max(v_pct),
                    _ => v_pct,
                },
                Mode::Charging => match k_pct {
                    Some(k) if !stuck => k,
                    // 内核卡死兜底：电压死推但封顶，防 CV 阶段虚高冲 100
                    _ => v_pct.min(cfg.charge_v_cap),
                },
                _ => sm.smooth,
            }
        };
        if mode == Mode::Discharging && v_comp_mv < cfg.valve_comp_mv {
            target = target.min(cfg.valve_cap);
        }

        // ---- 平滑限速推进（充电方向永不下调）----
        let dt = poll as f64;
        match mode {
            Mode::Discharging => {
                if target < sm.smooth {
                    let rate = if valve_active {
                        cfg.rate_valve as f64
                    } else if boost {
                        cfg.rate_dis_down as f64 / 2.0
                    } else {
                        cfg.rate_dis_down as f64
                    };
                    sm.smooth -= (dt / rate).min(sm.smooth - target);
                } else if target > sm.smooth {
                    sm.smooth += (dt / cfg.rate_dis_up as f64).min(target - sm.smooth);
                }
            }
            Mode::Charging => {
                if target > sm.smooth {
                    let rate = if stuck {
                        cfg.rate_charge_stuck as f64
                    } else {
                        cfg.rate_charge as f64
                    };
                    sm.smooth += (dt / rate).min(target - sm.smooth);
                }
            }
            _ => {}
        }
        sm.smooth = sm.smooth.clamp(cfg.min_percent as f64, 100.0);

        // ---- 满电：三者到顶（或系统报 Full）连续 3 拍 → reset 退出接管 ----
        if !full_latched && matches!(mode, Mode::Charging | Mode::Other) {
            let k_ok = k_pct.map_or(false, |k| k >= 99.0);
            let cond = (sm.smooth >= 99.0 && k_ok && v_pct >= 99.0) || status_str == "Full";
            full_count = if cond { full_count + 1 } else { 0 };
            if full_count >= 3 {
                write_log(&format!(
                    "满电确认 (smooth={:.1} k={:?} v={:.1} status={})，dumpsys battery reset 恢复真实电量",
                    sm.smooth, k_pct, v_pct, status_str
                ));
                let _ = Command::new("dumpsys").args(["battery", "reset"]).output();
                sm.smooth = k_pct.unwrap_or(100.0).min(100.0);
                full_latched = true;
                post_full_idle = true;
                full_count = 0;
                smoother::save(sm.smooth, &status_str);
            }
        } else if full_latched
            && (mode == Mode::Discharging || k_pct.map_or(false, |k| k < 97.0))
        {
            full_latched = false;
        }

        // ---- 下发电量：固定 30s 周期；切入放电时立即下发 ----
        let publishable = matches!(mode, Mode::Discharging | Mode::Charging) && !post_full_idle;
        let publish_due = now_ts.saturating_sub(last_pub_ts) >= PUBLISH_SECS;
        if publishable && (publish_due || force_publish) {
            let lvl = sm.smooth.floor().clamp(cfg.min_percent as f64, 100.0) as i64;
            let _ = Command::new("dumpsys")
                .args(["battery", "set", "level", &lvl.to_string()])
                .output();
            last_pub_ts = now_ts;
            force_publish = false;
            smoother::save(sm.smooth, &status_str);
            write_log(&format!(
                "set level {} | {} smooth={:.1} target={:.1} v={:.1}%(补偿{}mV/裸{}mV) k={:?}{}{}{}",
                lvl,
                mode.name(),
                sm.smooth,
                target,
                v_pct,
                v_comp_mv,
                v_mv_raw,
                k_pct,
                if stuck { " [内核不动]" } else { "" },
                if valve_active { " [安全阀]" } else { "" },
                if in_relax { " [弛豫]" } else { "" },
            ));
        }

        // ---- 校准打点（CALIB_LOG=true 时用于拟合 V_CURVE）----
        if cfg.calib_log && mode == Mode::Discharging && v_mv_raw > 0 {
            write_log(&format!(
                "CALIB v_comp={} v_raw={} i_ma={:?} k={:?}",
                v_comp_mv, v_mv_raw, rd.i_ma, k_pct
            ));
        }
    }
}
