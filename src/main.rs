use chrono::Local;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

// ===== 全局常量定义 =====
const LONG_SLEEP: u64 = 2; // 电量百分比更新时间间隔
const DISCHARGE_THRESHOLD: u64 = 15; // 持续放电时，2s的多少倍执行一次刷新
const MAX_RETRY: u32 = 3; // 命令重试次数

const LOG_FILE: &str = "/data/adb/battery_calibrate.log";
const BATTERY_PATH: &str = "/sys/class/power_supply/battery";
const BRIGHTNESS_PATHS: &[&str] = &[
    "/sys/class/backlight/panel0-backlight/brightness",
    "/sys/class/leds/lcd-backlight/brightness",
    "/sys/devices/platform/soc/soc:mtk_leds/leds/lcd-backlight/brightness",
];
const COUNTER_FILE: &str = "/data/adb/battery_calibrate.counter";
const MAX_CHARGE_COUNTER_FILE: &str = "/data/adb/battery_max_charge_counter";

// ===== 辅助工具函数 =====

fn now() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn write_log(msg: &str) {
    // 检查日志文件大小并截断 (> 5MB 保留最后1000行)
    if let Ok(metadata) = fs::metadata(LOG_FILE) {
        if metadata.len() >= 5 * 1024 * 1024 {
            if let Ok(content) = fs::read_to_string(LOG_FILE) {
                let lines: Vec<&str> = content.lines().collect();
                let keep_count = std::cmp::min(1000, lines.len());
                let keep_lines = &lines[lines.len() - keep_count..];
                let _ = fs::write(
                    LOG_FILE,
                    keep_lines.join("\n") + &format!("\n[{}] 日志文件超过5MB，已截断保留最新内容\n", now()),
                );
            }
        }
    }

    // 追加写入日志
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = writeln!(file, "[{}] {}", now(), msg);
    }
}

fn read_sys_file(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|_| "".to_string())
        .trim()
        .to_string()
}

fn read_sys_file_i64(path: &str) -> i64 {
    read_sys_file(path).parse::<i64>().unwrap_or(0)
}

fn log_exec(desc: &str, cmd: &str, args: &[&str]) -> bool {
    write_log(&format!("正在执行: {}", desc));
    for retry in 0..MAX_RETRY {
        match Command::new(cmd).args(args).output() {
            Ok(output) => {
                let status_success = output.status.success();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let out_str = format!("{}{}", stdout, stderr).trim().to_string();

                write_log(&format!("命令输出: {}", out_str));
                if status_success {
                    write_log("执行成功");
                    return true;
                }
            }
            Err(e) => write_log(&format!("命令执行异常: {}", e)),
        }
        sleep(Duration::from_secs(1));
    }
    write_log(&format!("执行失败 (尝试 {} 次)", MAX_RETRY));
    false
}

fn get_prop(prop: &str) -> String {
    let out = Command::new("getprop")
        .arg(prop)
        .output()
        .expect("Failed to execute getprop");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ===== 配置文件读取函数 =====
fn read_config(config_path: &str, key: &str, default: bool) -> bool {
    if let Ok(content) = fs::read_to_string(config_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if line.starts_with(key) {
                if let Some((_, val)) = line.split_once('=') {
                    // 支持 "true", "1", "yes" 等多种写法，忽略大小写
                    let val_lower = val.trim().to_lowercase();
                    return val_lower == "true" || val_lower == "1" || val_lower == "yes";
                }
            }
        }
    }
    default // 没找到文件或键值，返回默认值
}

// ===== 核心业务函数 =====

fn cancel_countdown() {
    write_log("正在尝试禁用电源服务(关闭30秒倒计时)...");
    let target_pkg = "com.miui.securitycenter/com.miui.powercenter.provider.PowerSaveService";
    let _ = Command::new("pm").args(&["disable", target_pkg]).output();

    sleep(Duration::from_secs(2));
    let out = Command::new("pm").args(&["list", "packages"]).output().unwrap();
    let pkg_list = String::from_utf8_lossy(&out.stdout);

    if !pkg_list.contains(target_pkg) {
        write_log("电源服务禁用成功");
    } else {
        write_log("首次禁用失败，尝试重新禁用...");
        let _ = Command::new("pm").args(&["enable", target_pkg]).output();
        sleep(Duration::from_secs(5));
        let _ = Command::new("pm").args(&["disable", target_pkg]).output();

        let out_final = Command::new("pm").args(&["list", "packages"]).output().unwrap();
        if !String::from_utf8_lossy(&out_final.stdout).contains(target_pkg) {
            write_log("电源服务最终禁用成功");
        } else {
            write_log("电源服务禁用失败");
        }
    }
}

fn wait_for_batterystats() {
    write_log("等待电池服务启动中，需等待1分钟...");
    let start_time = Instant::now();
    let mut last_log_time = start_time;

    loop {
        let elapsed = start_time.elapsed().as_secs();
        let current_time = Instant::now();

        if current_time.duration_since(last_log_time).as_secs() >= 60 {
            let remaining = if elapsed >= 60 { 0 } else { 60 - elapsed };
            write_log(&format!("已等待 {} 分钟，还剩 {} 分钟...", elapsed / 60, remaining / 60));
            last_log_time = current_time;
        }

        if elapsed >= 60 {
            write_log("等待电池服务启动完成");
            break;
        }
        sleep(Duration::from_secs(1));
    }
}

fn monitor_voltage() {
    let mut last_status = String::new();
    let mut discharge_counter = 0;
    let mut in_full_state = false;
    let mut temp_max_charge = 0;

    // 初次获取最大电池容量
    let mut max_charge_counter = read_sys_file_i64(MAX_CHARGE_COUNTER_FILE);
    if max_charge_counter == 0 {
        max_charge_counter = read_sys_file_i64(&format!("{}/charge_full", BATTERY_PATH));
        let _ = fs::write(MAX_CHARGE_COUNTER_FILE, max_charge_counter.to_string());
    }

    let mut max_charge_counter_mah = if max_charge_counter > 20000 {
        max_charge_counter / 1000
    } else {
        max_charge_counter
    };
    write_log(&format!("初次获取最大电池容量:{}mAh", max_charge_counter_mah));

    // 无限循环监控
    loop {
        let loop_start = Instant::now();

        let charge_counter_raw = read_sys_file_i64(&format!("{}/charge_counter", BATTERY_PATH));
        let charge_counter_mah = charge_counter_raw; // 不强制换算，保持 Shell 脚本原生逻辑计算
        let capacity = read_sys_file_i64(&format!("{}/capacity", BATTERY_PATH));
        let charging_status = read_sys_file(&format!("{}/status", BATTERY_PATH));

        // 更新最大电池容量逻辑
        match charging_status.as_str() {
            "Not charging" | "Full" => {
                if capacity == 100 {
                    if !in_full_state {
                        max_charge_counter = charge_counter_raw;
                        let _ = fs::write(MAX_CHARGE_COUNTER_FILE, max_charge_counter.to_string());
                        temp_max_charge = charge_counter_raw;
                        in_full_state = true;

                        max_charge_counter_mah = if max_charge_counter > 20000 { max_charge_counter / 1000 } else { max_charge_counter };
                        write_log(&format!("电池首次充满，更新最大电池容量:{}mAh", max_charge_counter_mah));
                    } else if charge_counter_raw != temp_max_charge {
                        max_charge_counter = charge_counter_raw;
                        temp_max_charge = charge_counter_raw;
                        let _ = fs::write(MAX_CHARGE_COUNTER_FILE, max_charge_counter.to_string());

                        max_charge_counter_mah = if max_charge_counter > 20000 { max_charge_counter / 1000 } else { max_charge_counter };
                        write_log(&format!("持续充满中，更新最大电池容量:{}mAh", max_charge_counter_mah));
                    }
                } else {
                    in_full_state = false;
                }
            }
            _ => in_full_state = false,
        }

        // 获取屏幕亮度
        let mut brightness = 0;
        for path in BRIGHTNESS_PATHS {
            let val = read_sys_file_i64(path);
            if val > 0 {
                brightness = val;
                break;
            }
        }

        // 状态对
        let status_pair = format!("{}:{}", last_status, charging_status);

        // 计算百分比的闭包 (Rust 中没有 awk，直接用整数乘除法实现)
        let mut calculate_level = || -> i64 {
            if max_charge_counter == 0 { return 50; }
            let mut level = (charge_counter_mah * 100) / max_charge_counter;
            if level <= 0 { level = 5; }
            if level > 100 { level = 100; }
            level
        };

        if brightness > 0 {
            // 亮屏状态
            match status_pair.as_str() {
                "Discharging:Charging" => {
                    let _ = Command::new("dumpsys").args(&["battery", "reset"]).output();
                    write_log(&format!("放电→充电 | 系统电量:{}% | 当前容量:{}mAh | 最大容量:{}mAh", capacity, charge_counter_mah, max_charge_counter_mah));
                    discharge_counter = 0;
                }
                "Charging:Discharging" => {
                    let level = calculate_level();
                    let _ = Command::new("dumpsys").args(&["battery", "set", "level", &level.to_string()]).output();
                    write_log(&format!("充电→放电 | 更新电量:{}% | 系统电量:{}% | 最大容量:{}mAh", level, capacity, max_charge_counter_mah));
                    discharge_counter = 0;
                }
                "Discharging:Discharging" => {
                    discharge_counter += 1;
                    if discharge_counter % DISCHARGE_THRESHOLD == 0 {
                        let level = calculate_level();
                        let _ = Command::new("dumpsys").args(&["battery", "set", "level", &level.to_string()]).output();
                        write_log(&format!("持续放电 | 更新电量:{}% | 系统电量:{}% | 最大容量:{}mAh", level, capacity, max_charge_counter_mah));
                    }
                }
                "Charging:Charging" => {
                    // 持续充电中，不作为
                }
                _ => {}
            }
        } else {
            // 息屏状态
            if last_status == "Discharging" && charging_status == "Charging" {
                let _ = Command::new("dumpsys").args(&["battery", "reset"]).output();
                write_log(&format!("[息屏]放电→充电 | 系统电量:{}% | 最大容量:{}mAh", capacity, max_charge_counter_mah));
                discharge_counter = 0;
            }
        }

        last_status = charging_status;

        // 计算循环耗时，保持准确的2秒执行间隔
        let elapsed = loop_start.elapsed().as_secs();
        let remaining = if LONG_SLEEP > elapsed { LONG_SLEEP - elapsed } else { 1 };
        sleep(Duration::from_secs(remaining));
    }
}

// ===========================================
// ============ 主执行流程 ====================
// ===========================================
fn main() {
    // 0. 解析参数获取配置文件路径
    let args: Vec<String> = env::args().collect();
    // service.sh 应该把 $MODDIR 作为第一个参数传过来
    let mod_dir = if args.len() > 1 {
        args[1].clone()
    } else {
        "/data/adb/modules/battery_module".to_string() // 后备路径
    };
    let config_file = format!("{}/config.conf", mod_dir);

    // 1. 读取配置文件状态 (默认开启 true)
    let enable_monitor = read_config(&config_file, "ENABLE_MONITOR", true);
    let enable_temp_comp = read_config(&config_file, "ENABLE_TEMP_COMP", true);

    write_log("");
    write_log("============ 模块启动 ==============");
    write_log(&format!("配置文件路径: {}", config_file));
    write_log(&format!("配置[电量更新监控]: {}", if enable_monitor { "开启" } else { "禁用" }));
    write_log(&format!("配置[温度补偿限制]: {}", if enable_temp_comp { "开启" } else { "禁用" }));

    // 2. Root 权限检查
    write_log("第一步：正在验证Root权限...");
    let out = Command::new("id").arg("-u").output().unwrap_or_else(|_| panic!("Failed to run id command"));
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid != "0" {
        write_log("错误：需要Root权限执行! 程序退出。");
        return;
    } else {
        write_log("Root权限验证通过");
    }

    // 打印设备信息
    write_log("============ 设备信息 ==============");
    write_log(&format!("设备型号: {}", get_prop("ro.product.model")));
    write_log(&format!("系统版本: {}", get_prop("ro.build.version.incremental")));
    let voltage_now = read_sys_file_i64(&format!("{}/voltage_now", BATTERY_PATH));
    write_log(&format!("当前电压: {:.3}V", voltage_now as f64 / 1_000_000.0));
    write_log(&format!("当前电量: {}%", read_sys_file(&format!("{}/capacity", BATTERY_PATH))));
    write_log(&format!("充电状态: {}", read_sys_file(&format!("{}/status", BATTERY_PATH))));
    write_log(&format!("电池健康: {}", read_sys_file(&format!("{}/health", BATTERY_PATH))));

    // 3. 关闭30秒倒计时
    write_log("第二步：正在关闭30秒倒计时关机提醒...");
    cancel_countdown();

    // 4. 禁用系统保护机制
    write_log("第三步：正在配置系统保护机制与电池老化因子...");
    if enable_temp_comp {
        log_exec("禁用温度补偿", "setprop", &["persist.vendor.power.disable_temp_comp", "1"]);
    } else {
        write_log("用户已配置：跳过禁用温度补偿");
    }
    log_exec("禁用电压补偿", "setprop", &["persist.vendor.power.disable_voltage_comp", "1"]);
    log_exec("设置老化因子为100", "setprop", &["persist.vendor.battery.age_factor", "100"]);

    // 5. 计数器处理与电池统计重置
    write_log("第四步：正在处理电池统计信息...");
    let reboot_count = read_sys_file_i64(COUNTER_FILE);
    let new_count = reboot_count + 1;
    let _ = fs::write(COUNTER_FILE, new_count.to_string());

    write_log(&format!("当前手机重启 {} 次", new_count));
    write_log("手机重启次数为60的倍数时，才执行\"重置电池统计信息\"");

    if new_count % 60 == 0 {
        wait_for_batterystats();
        log_exec("重置统计信息", "dumpsys", &["batterystats", "--reset"]);
        log_exec("发送重置广播", "am", &["broadcast", "-a", "com.xiaomi.powercenter.RESET_STATS"]);
        let _ = fs::remove_file("/data/system/batterystats.bin");
        write_log("删除统计文件完成");
    }

    write_log("========= 电池续航延长操作初始化完成 ===========");

    // 6. 是否进入循环监控
    if enable_monitor {
        write_log("============= 开始更新电量 ===============");
        monitor_voltage(); // 此处进入死循环，不退出
    } else {
        write_log("============= 运行结束 ===============");
        write_log("用户已配置：禁用电量百分比更新监控，Rust后台服务安全退出。");
    }
}
