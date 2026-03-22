use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

// 修复点1：添加 TimerSetTimeFlags 导入
use nix::sys::timerfd::{ClockId, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};

const LONG_SLEEP: u64 = 3;
const DISCHARGE_THRESHOLD: u64 = 10;
const MAX_RETRY: u32 = 3;

const LOG_FILE: &str = "/data/adb/battery_calibrate.log";
const BATTERY_PATH: &str = "/sys/class/power_supply/battery";
const BRIGHTNESS_PATHS: &[&str] = &[
    "/sys/class/backlight/panel0-backlight/brightness",
    "/sys/class/leds/lcd-backlight/brightness",
    "/sys/devices/platform/soc/soc:mtk_leds/leds/lcd-backlight/brightness",
];
const COUNTER_FILE: &str = "/data/adb/battery_calibrate.counter";
const MAX_CHARGE_COUNTER_FILE: &str = "/data/adb/battery_max_charge_counter";

// 日志清理核心配置（仅保留时间间隔，路径改为动态）
const LOG_CLEAN_INTERVAL_SECS: u64 = 3 * 24 * 60 * 60; // 3天 = 259200秒

static TIME_FMT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");

// 获取当前Unix时间戳（秒）
fn get_current_unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// 读取最后一次清理的时间戳（动态路径：程序根目录下）
fn read_last_clean_ts(mod_dir: &str) -> u64 {
    let last_clean_file = format!("{}/battery_calibrate.last_clean", mod_dir);
    fs::read_to_string(last_clean_file)
        .unwrap_or_default()
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
}

// 更新最后一次清理的时间戳（动态路径：程序根目录下）
fn update_last_clean_ts(mod_dir: &str) {
    let current_ts = get_current_unix_ts();
    let last_clean_file = format!("{}/battery_calibrate.last_clean", mod_dir);
    let _ = fs::write(last_clean_file, current_ts.to_string());
}

// 强制清空日志文件（无任何额外写入）
fn force_clean_log() {
    // 直接创建空文件覆盖原有日志，无任何写入操作
    let _ = File::create(LOG_FILE);
}

// 检查是否需要执行3天一次的日志清理（仅启动时执行，动态路径）
fn check_and_clean_log_periodically(mod_dir: &str) {
    let last_clean_ts = read_last_clean_ts(mod_dir);
    let current_ts = get_current_unix_ts();
    let time_diff = current_ts.saturating_sub(last_clean_ts);

    // 仅当时间差≥3天时，执行清理，否则无任何操作
    if time_diff >= LOG_CLEAN_INTERVAL_SECS {
        force_clean_log();
        update_last_clean_ts(mod_dir); // 清理后更新时间戳（程序根目录）
    }
}

fn now() -> String {
    let dt = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    dt.format(TIME_FMT)
        .unwrap_or_else(|_| "time_err".to_string())
}

// 简化后的写日志函数（移除所有截断逻辑）
fn write_log(msg: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = writeln!(f, "[{}] {}", now(), msg);
    }
}

fn read_sys_file(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn read_sys_file_i64(path: &str) -> i64 {
    read_sys_file(path).parse::<i64>().unwrap_or(0)
}

fn log_exec(desc: &str, cmd: &str, args: &[&str]) -> bool {
    write_log(&format!("正在执行: {}", desc));
    for _ in 0..MAX_RETRY {
        match Command::new(cmd).args(args).output() {
            Ok(output) => {
                if output.status.success() {
                    write_log("执行成功");
                    return true;
                }
            }
            Err(e) => {
                write_log(&format!("命令执行异常: {}", e));
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    write_log(&format!("执行失败 (尝试 {} 次)", MAX_RETRY));
    false
}

fn get_prop(prop: &str) -> String {
    match Command::new("getprop").arg(prop).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => "".to_string(),
    }
}

fn cancel_countdown() {
    write_log("正在尝试禁用电源服务(关闭30秒倒计时)...");
    let target_pkg = "com.miui.securitycenter/com.miui.powercenter.provider.PowerSaveService";
    let _ = Command::new("pm").args(&["disable", target_pkg]).output();

    std::thread::sleep(Duration::from_secs(2));
    if let Ok(out) = Command::new("pm").args(&["list", "packages"]).output() {
        let pkg_list = String::from_utf8_lossy(&out.stdout);
        if !pkg_list.contains(target_pkg) {
            write_log("电源服务禁用成功");
            return;
        }
    }

    write_log("首次禁用失败，尝试重新禁用...");
    let _ = Command::new("pm").args(&["enable", target_pkg]).output();
    std::thread::sleep(Duration::from_secs(5));
    let _ = Command::new("pm").args(&["disable", target_pkg]).output();

    if let Ok(out_final) = Command::new("pm").args(&["list", "packages"]).output() {
        if !String::from_utf8_lossy(&out_final.stdout).contains(target_pkg) {
            write_log("电源服务最终禁用成功");
        } else {
            write_log("电源服务禁用失败");
        }
    } else {
        write_log("检查包列表失败");
    }
}

fn wait_for_batterystats() {
    write_log("等待电池服务启动中，需等待1分钟...");
    let start = SystemTime::now();
    let mut last_log = start;
    loop {
        let elapsed = SystemTime::now()
            .duration_since(start)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let since_last = SystemTime::now()
            .duration_since(last_log)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if since_last >= 60 {
            let remaining = if elapsed >= 60 { 0 } else { 60 - elapsed };
            write_log(&format!(
                "已等待 {} 分钟，还剩 {} 分钟...",
                elapsed / 60,
                remaining / 60
            ));
            last_log = SystemTime::now();
        }
        if elapsed >= 60 {
            write_log("等待电池服务启动完成");
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// **兼顾原版校准逻辑与动态更新的 TimerFd 版本（极致 I/O 优化）**
fn monitor_voltage() {
    let mut last_status = String::new();
    let mut discharge_counter: u64 = 0;
    let mut tick_count: u64 = 0; // 用于降低低频数据的读取频率

    // 原版的满电状态追踪变量
    let mut in_full_state = false;
    let mut temp_max_charge: i64 = 0;

    // 读取历史缓存的最大容量
    let mut cached_max_charge = read_sys_file_i64(MAX_CHARGE_COUNTER_FILE);

    // [优化] 在循环外先获取一次初始的 sys_charge_full 基准值
    let mut sys_charge_full = read_sys_file_i64(&format!("{}/charge_full", BATTERY_PATH));
    if sys_charge_full > 100_000 {
        sys_charge_full /= 1000;
    }
    if sys_charge_full <= 0 {
        sys_charge_full = 4000;
    } // 极端情况兜底

    // 创建长期存在的 TimerFd（阻塞）
    let tfd =
        TimerFd::new(ClockId::CLOCK_MONOTONIC, TimerFlags::empty()).expect("TimerFd create failed");

    tfd.set(
        Expiration::Interval(Duration::from_secs(LONG_SLEEP).into()),
        TimerSetTimeFlags::empty(),
    )
    .expect("TimerFd set failed");

    loop {
        // 阻塞等待 LONG_SLEEP 秒
        let _ = tfd.wait().expect("TimerFd wait failed");
        tick_count = tick_count.wrapping_add(1);

        // 1. [高频数据] 每 3 秒必须动态获取的实时变动数据
        let mut sys_charge_counter = read_sys_file_i64(&format!("{}/charge_counter", BATTERY_PATH));
        if sys_charge_counter > 100_000 {
            sys_charge_counter /= 1000;
        }

        let capacity = read_sys_file_i64(&format!("{}/capacity", BATTERY_PATH));
        let charging_status = read_sys_file(&format!("{}/status", BATTERY_PATH));

        // 2. [低频数据] 根据用户建议，仅在达到 DISCHARGE_THRESHOLD (约 30 秒) 时才读取一次 charge_full，减少 I/O 损耗
        if tick_count % DISCHARGE_THRESHOLD == 0 {
            let raw_charge_full = read_sys_file_i64(&format!("{}/charge_full", BATTERY_PATH));
            if raw_charge_full > 100_000 {
                sys_charge_full = raw_charge_full / 1000;
            } else if raw_charge_full > 0 {
                sys_charge_full = raw_charge_full;
            }
        }

        // 本次循环使用的最终最大容量
        let mut current_max_charge;

        // 3. 核心：完美保留原版的手动校准逻辑 (追踪涓流充电峰值，依赖高频的 sys_charge_counter)
        match charging_status.as_str() {
            "Not charging" | "Full" => {
                if capacity == 100 {
                    if !in_full_state {
                        cached_max_charge = sys_charge_counter;
                        let _ = fs::write(MAX_CHARGE_COUNTER_FILE, cached_max_charge.to_string());
                        temp_max_charge = sys_charge_counter;
                        in_full_state = true;
                    } else if sys_charge_counter != temp_max_charge {
                        cached_max_charge = sys_charge_counter;
                        temp_max_charge = sys_charge_counter;
                        let _ = fs::write(MAX_CHARGE_COUNTER_FILE, cached_max_charge.to_string());
                    }
                    current_max_charge = cached_max_charge;
                } else {
                    in_full_state = false;
                    current_max_charge = cached_max_charge;
                }
            }
            _ => {
                in_full_state = false;
                current_max_charge = cached_max_charge;
            }
        }

        // 4. 解决“死板缓存”问题 (弹性同步机制)
        // 使用降频读取的 sys_charge_full 来校验缓存是否已严重偏离底层实际数据
        if current_max_charge <= 0 || (current_max_charge - sys_charge_full).abs() > 200 {
            current_max_charge = sys_charge_full;
            cached_max_charge = sys_charge_full;
        }

        // 防御性安全校验
        if current_max_charge <= 0 {
            current_max_charge = 4000;
        }

        let mut brightness = 0i64;
        for path in BRIGHTNESS_PATHS {
            if Path::new(path).exists() {
                brightness = read_sys_file_i64(path);
                break;
            }
        }

        // 5. 使用正确的实时变量进行百分比计算
        let calculate_level = || -> i64 {
            let mut level = sys_charge_counter.saturating_mul(100) / current_max_charge;
            if level <= 0 {
                level = 1;
            }
            if level > 100 {
                level = 100;
            }
            level
        };

        if brightness > 0 {
            match (last_status.as_str(), charging_status.as_str()) {
                ("Discharging", "Charging") => {
                    let _ = Command::new("dumpsys").args(&["battery", "reset"]).output();
                    write_log(&format!(
                        "放电→充电 | 系统电量:{}% | 动态最大容量:{}mAh | 当前容量:{}mAh",
                        capacity, current_max_charge, sys_charge_counter
                    ));
                    discharge_counter = 0;
                }
                ("Charging", "Discharging") => {
                    let level = calculate_level();
                    let _ = Command::new("dumpsys")
                        .args(&["battery", "set", "level", &level.to_string()])
                        .output();
                    write_log(&format!("充电→放电 | 更新电量:{}% | 系统电量:{}% | 动态最大容量:{}mAh | 当前容量:{}mAh",
                        level, capacity, current_max_charge, sys_charge_counter));
                    discharge_counter = 0;
                }
                ("Discharging", "Discharging") => {
                    discharge_counter += 1;
                    if discharge_counter % DISCHARGE_THRESHOLD == 0 {
                        let level = calculate_level();
                        let _ = Command::new("dumpsys")
                            .args(&["battery", "set", "level", &level.to_string()])
                            .output();
                        write_log(&format!("持续放电 | 更新电量:{}% | 系统电量:{}% | 动态最大容量:{}mAh | 当前容量:{}mAh",
                            level, capacity, current_max_charge, sys_charge_counter));
                    }
                }
                _ => {}
            }
        } else {
            if last_status == "Discharging" && charging_status == "Charging" {
                let _ = Command::new("dumpsys").args(&["battery", "reset"]).output();
                write_log(&format!(
                    "[息屏]放电→充电 | 系统电量:{}% | 动态最大容量:{}mAh | 当前容量:{}mAh",
                    capacity, current_max_charge, sys_charge_counter
                ));
                discharge_counter = 0;
            }
        }

        last_status = charging_status;
    }
}

fn read_config_bool(config_path: &str, key: &str, default: bool) -> bool {
    if let Ok(content) = fs::read_to_string(config_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if line.starts_with(key) {
                if let Some((_, val)) = line.split_once('=') {
                    let val_lower = val.trim().to_lowercase();
                    return val_lower == "true" || val_lower == "1" || val_lower == "yes";
                }
            }
        }
    }
    default
}

fn handle_counter() -> i64 {
    let reboot_count = read_sys_file_i64(COUNTER_FILE);
    let new_count = reboot_count + 1;
    let _ = fs::write(COUNTER_FILE, new_count.to_string());
    new_count
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mod_dir = if args.len() > 1 {
        args[1].clone()
    } else {
        "/data/adb/modules/battery_module".to_string()
    };
    let config_file = format!("{}/config.conf", mod_dir);

    // 仅启动时检查一次日志清理（时间戳文件在程序根目录）
    check_and_clean_log_periodically(&mod_dir);

    let enable_monitor = read_config_bool(&config_file, "ENABLE_MONITOR", true);
    let enable_temp_comp = read_config_bool(&config_file, "ENABLE_TEMP_COMP", true);

    write_log("");
    write_log("============ 模块启动 ==============");
    write_log(&format!("配置文件路径: {}", config_file));
    write_log(&format!(
        "配置[电量更新监控]: {}",
        if enable_monitor { "开启" } else { "禁用" }
    ));
    write_log(&format!(
        "配置[温度补偿限制]: {}",
        if enable_temp_comp { "开启" } else { "禁用" }
    ));

    write_log("第一步：正在验证Root权限...");
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    if uid != "0" {
        write_log("错误：需要Root权限执行! 程序退出。");
        return;
    } else {
        write_log("Root权限验证通过");
    }

    write_log("============ 设备信息 ==============");
    write_log(&format!("设备型号: {}", get_prop("ro.product.model")));
    write_log(&format!(
        "系统版本: {}",
        get_prop("ro.build.version.incremental")
    ));
    let voltage_now = read_sys_file_i64(&format!("{}/voltage_now", BATTERY_PATH));
    write_log(&format!(
        "当前电压: {:.3}V",
        voltage_now as f64 / 1_000_000.0
    ));
    write_log(&format!(
        "当前电量: {}%",
        read_sys_file(&format!("{}/capacity", BATTERY_PATH))
    ));
    write_log(&format!(
        "充电状态: {}",
        read_sys_file(&format!("{}/status", BATTERY_PATH))
    ));
    write_log(&format!(
        "电池健康: {}",
        read_sys_file(&format!("{}/health", BATTERY_PATH))
    ));

    write_log("第二步：正在关闭30秒倒计时关机提醒...");
    cancel_countdown();

    write_log("第三步：正在配置系统保护机制与电池老化因子...");
    if enable_temp_comp {
        log_exec(
            "禁用温度补偿",
            "setprop",
            &["persist.vendor.power.disable_temp_comp", "1"],
        );
    } else {
        write_log("用户已配置：跳过禁用温度补偿");
    }
    log_exec(
        "禁用电压补偿",
        "setprop",
        &["persist.vendor.power.disable_voltage_comp", "1"],
    );
    log_exec(
        "设置老化因子为100",
        "setprop",
        &["persist.vendor.battery.age_factor", "100"],
    );

    write_log("第四步：正在处理电池统计信息...");
    let reboot_count = handle_counter();
    write_log(&format!("当前手机重启 {} 次", reboot_count));
    write_log("手机重启次数为60的倍数时，才执行\"重置电池统计信息\"");

    if reboot_count % 60 == 0 {
        wait_for_batterystats();
        log_exec("重置统计信息", "dumpsys", &["batterystats", "--reset"]);
        log_exec(
            "发送重置广播",
            "am",
            &["broadcast", "-a", "com.xiaomi.powercenter.RESET_STATS"],
        );
        let _ = fs::remove_file("/data/system/batterystats.bin");
        write_log("删除统计文件完成");
    }

    write_log("========= 电池续航延长操作初始化完成 ===========");

    if enable_monitor {
        write_log("============= 开始更新电量 ===============");
        monitor_voltage();
    } else {
        write_log("============= 运行结束 ===============");
        write_log("用户已配置：禁用电量百分比更新监控，Rust后台服务安全退出。");
    }
}
