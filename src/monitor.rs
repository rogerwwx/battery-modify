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
/// 放电时内核与电压偏差超过该值视为小板异常，回落纯电压
const KERNEL_SANITY_GAP: f64 = 25.0;
/// 内核电量累计漂移达到该值视为“在动”（RM/FCC 是连续量，不能按 1% 整数跳变判断）
const KERNEL_MOVE_EPS: f64 = 0.5;
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

/// 读取系统当前显示电量（仅用于接管初始化，防开机跳变）。
/// 开机早期 dumpsys 可能未就绪，重试至多 10 次。
fn read_displayed_level() -> Option<i64> {
    for _ in 0..10 {
        if let Ok(out) = Command::new("dumpsys").arg("battery").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            for line in s.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("level:") {
                    if let Ok(v) = rest.trim().parse::<i64>() {
                        return Some(v);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
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
    let mut k_mark: Option<f64> = None;
    let mut k_accepted: Option<f64> = None;
    let mut k_pending: Option<f64> = None;

    let mut valve_count: u32 = 0;
    let mut valve_active = false;

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
                        // 充电期间未接管，系统显示即真实电量：以它为接管起点，不跳变
                        let displayed = read_displayed_level();
                        sm.smooth = match displayed {
                            Some(d) => d as f64,
                            None => k_accepted.unwrap_or(sm.smooth),
                        };
                        k_mark = None;
                        force_publish = true;
                        write_log(&format!(
                            "切换→放电 | 接管起点 smooth={:.1}（系统显示 {:?}，内核 {:?}）",
                            sm.smooth, displayed, k_accepted
                        ));
                    }
                    Mode::Charging => {
                        // 充电期间不接管：交还系统计电量，快充显示实时跟上
                        let _ = Command::new("dumpsys").args(["battery", "reset"]).output();
                        valve_count = 0;
                        valve_active = false;
                        write_log("切换→充电 | dumpsys battery reset，充电期间由系统计电量");
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
        // RM/FCC 是连续量，每拍只漂移零点几个百分点；
        // 按“累计漂移 ≥0.5%”判断在动，避免把正常漂移误判成卡死
        if let Some(k) = k_pct {
            match k_mark {
                None => {
                    k_mark = Some(k);
                    last_k_move = now_ts;
                }
                Some(m) => {
                    if (k - m).abs() >= KERNEL_MOVE_EPS {
                        k_mark = Some(k);
                        last_k_move = now_ts;
                    }
                }
            }
        }
        let stuck = now_ts.saturating_sub(last_k_move) > cfg.stuck_timeout_secs;

        let in_relax = mode == Mode::Discharging && now_ts < relax_until;
        let boost = mode == Mode::Discharging && v_pct < sm.smooth - 10.0;

        // ---- 目标融合 ----
        let mut target = if valve_active && mode == Mode::Discharging {
            cfg.min_percent as f64
        } else {
            match mode {
                Mode::Discharging => {
                    // 以电压模拟为准，内核做下限保护（max）；
                    // 内核缺失、卡死或与电压偏差离谱（小板异常）时不参与融合
                    let k_ok = match k_pct {
                        Some(k) => !stuck && (k - v_pct).abs() <= KERNEL_SANITY_GAP,
                        None => false,
                    };
                    if in_relax {
                        // 拔线弛豫窗口：电压被表面电荷抬高，暂以内核为准
                        k_pct.unwrap_or(v_pct)
                    } else if k_ok {
                        v_pct.max(k_pct.unwrap())
                    } else {
                        v_pct
                    }
                }
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
                    // 大幅落后（如开机时系统显示是过期值）按下降同速追赶；
                    // 小幅差距（负载移除后的电压回弹）仍用慢速回升
                    let rate = if target - sm.smooth > 5.0 {
                        cfg.rate_dis_down as f64
                    } else {
                        cfg.rate_dis_up as f64
                    };
                    sm.smooth += (dt / rate).min(target - sm.smooth);
                }
            }
            _ => {}
        }
        sm.smooth = sm.smooth.clamp(cfg.min_percent as f64, 100.0);

        // ---- 下发电量：固定 30s 周期；切入放电时立即下发 ----
        let publishable = mode == Mode::Discharging;
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
                "set level {} | {} smooth={:.1} target={:.1} v={:.1}%(补偿{}mV/裸{}mV) k={:.1} rm={:.0}/fcc={:.0}mAh{}{}{}",
                lvl,
                mode.name(),
                sm.smooth,
                target,
                v_pct,
                v_comp_mv,
                v_mv_raw,
                k_pct.unwrap_or(-1.0),
                rd.rm_mah.unwrap_or(0.0),
                rd.fcc_mah.unwrap_or(0.0),
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
