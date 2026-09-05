use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

pub const LOG_FILE: &str = "/data/adb/battery_calibrate.log";
pub const COUNTER_FILE: &str = "/data/adb/battery_calibrate.counter";
const LOG_CLEAN_INTERVAL_SECS: u64 = 3 * 24 * 60 * 60; // 3天

static TIME_FMT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");

pub fn get_current_unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

pub fn now() -> String {
    let dt = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    dt.format(TIME_FMT)
        .unwrap_or_else(|_| "time_err".to_string())
}

pub fn write_log(msg: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = writeln!(f, "[{}] {}", now(), msg);
    }
}

pub fn read_sys_file(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn read_sys_file_i64(path: &str) -> i64 {
    read_sys_file(path).parse::<i64>().unwrap_or(0)
}

pub fn log_exec(desc: &str, cmd: &str, args: &[&str]) -> bool {
    write_log(&format!("正在执行: {}", desc));
    for _ in 0..3 {
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
    write_log(&format!("执行失败 (尝试 3 次)"));
    false
}

pub fn get_prop(prop: &str) -> String {
    match Command::new("getprop").arg(prop).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => "".to_string(),
    }
}

fn read_last_clean_ts(mod_dir: &str) -> u64 {
    let last_clean_file = format!("{}/battery_calibrate.last_clean", mod_dir);
    fs::read_to_string(last_clean_file)
        .unwrap_or_default()
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
}

fn update_last_clean_ts(mod_dir: &str) {
    let last_clean_file = format!("{}/battery_calibrate.last_clean", mod_dir);
    let _ = fs::write(last_clean_file, get_current_unix_ts().to_string());
}

fn force_clean_log() {
    let _ = File::create(LOG_FILE);
}

pub fn check_and_clean_log_periodically(mod_dir: &str) {
    let last_clean_ts = read_last_clean_ts(mod_dir);
    let time_diff = get_current_unix_ts().saturating_sub(last_clean_ts);
    if time_diff >= LOG_CLEAN_INTERVAL_SECS {
        force_clean_log();
        update_last_clean_ts(mod_dir);
    }
}

pub fn handle_counter() -> i64 {
    let reboot_count = read_sys_file_i64(COUNTER_FILE);
    let new_count = reboot_count + 1;
    let _ = fs::write(COUNTER_FILE, new_count.to_string());
    new_count
}
