use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

mod config;
mod filter;
mod monitor;
mod smoother;
mod source;
mod util;
mod vcurve;

use crate::config::Config;
use crate::source::BATTERY_PATH;
use crate::util::{
    check_and_clean_log_periodically, get_prop, handle_counter, log_exec, read_sys_file,
    read_sys_file_i64, write_log,
};

fn cancel_countdown() {
    write_log("正在尝试禁用电源服务(关闭30秒倒计时)...");
    let target_pkg = "com.miui.securitycenter/com.miui.powercenter.provider.PowerSaveService";
    let _ = Command::new("pm").args(["disable", target_pkg]).output();

    thread::sleep(Duration::from_secs(2));
    if let Ok(out) = Command::new("pm").args(["list", "packages"]).output() {
        let pkg_list = String::from_utf8_lossy(&out.stdout);
        if !pkg_list.contains(target_pkg) {
            write_log("电源服务禁用成功");
            return;
        }
    }

    write_log("首次禁用失败，尝试重新禁用...");
    let _ = Command::new("pm").args(["enable", target_pkg]).output();
    thread::sleep(Duration::from_secs(5));
    let _ = Command::new("pm").args(["disable", target_pkg]).output();

    if let Ok(out_final) = Command::new("pm").args(["list", "packages"]).output() {
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
        thread::sleep(Duration::from_secs(1));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mod_dir = if args.len() > 1 {
        args[1].clone()
    } else {
        "/data/adb/modules/battery_module".to_string()
    };
    let config_file = format!("{}/config.conf", mod_dir);

    // 仅启动时检查一次日志清理（时间戳文件在模块根目录）
    check_and_clean_log_periodically(&mod_dir);

    let cfg = Config::load(&config_file);

    write_log("");
    write_log("============ 模块启动 ==============");
    write_log(&format!("配置文件路径: {}", config_file));
    write_log(&format!(
        "配置[电量更新监控]: {}",
        if cfg.enable_monitor { "开启" } else { "禁用" }
    ));
    write_log(&format!(
        "配置[温度补偿限制]: {}",
        if cfg.enable_temp_comp { "开启" } else { "禁用" }
    ));
    write_log(&format!(
        "参数: R={:.0}mΩ | 放电1%每{}s 回升1%每{}s 充电1%每{}s 内核不动1%每{}s 安全阀1%每{}s",
        cfg.r_mohm, cfg.rate_dis_down, cfg.rate_dis_up, cfg.rate_charge,
        cfg.rate_charge_stuck, cfg.rate_valve
    ));
    write_log(&format!(
        "参数: 轮询={}s 卡死超时={}s 弛豫={}s 安全阀<{}mV 充电电压封顶={:.0}%",
        cfg.poll_secs, cfg.stuck_timeout_secs, cfg.relax_secs, cfg.valve_mv, cfg.charge_v_cap
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
    if cfg.enable_temp_comp {
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

    if cfg.enable_monitor {
        write_log("============= 开始更新电量 ===============");
        monitor::run(&cfg);
    } else {
        write_log("============= 运行结束 ===============");
        write_log("用户已配置：禁用电量百分比更新监控，Rust后台服务安全退出。");
    }
}
