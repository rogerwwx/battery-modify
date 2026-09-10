# battery-modify

Android (Magisk 模块) 电量接管守护进程：用电池电压模拟电量，融合内核 fuel gauge 的 RM/FCC，
经平滑限速后通过 `dumpsys battery set level` 下发，避免小板错误数据导致的电量跳变。

## 工作原理

- 按 `POLL_SECS`（默认 10s）轮询读取 `voltage_now` / `current_now` / `status` / RM / FCC（节点按实机写死：`/sys/class/power_supply/battery/` 下的 `voltage_now`/`current_now`/`charge_counter`/`charge_full`）
- 放电时按内阻做负载补偿（`v + |I|×R`），查分段线性表得 voltage_percent，经中位数+EMA 降噪
- kernel_percent = RM × 100 / FCC，带毛刺拒绝（单拍跳变 >8% 需下一拍确认）
- 放电：以电压模拟为准，`target = max(voltage_percent, kernel_percent)` 做下限保护；内核缺失、卡死或与电压偏差 >25 个点（小板异常）时不参与融合；拔线弛豫窗口内跟随内核
- 充电：**不接管**——`dumpsys battery reset` 交还系统计电量，快充显示实时跟上；拔线后以系统当前显示值为起点重新接管，不跳变
- smooth_percent 按时间基限速逼近 target（各方向速率独立可配），每 30s 下发一次（与旧版频率一致）
- 低电安全阀：端电压低于阈值（裸电压判定）快速下探，防止电压曲线失配导致直接关机
- smooth 值持久化到 `/data/adb/battery_smooth.state`，daemon 重启续跑不跳变

## config.conf

位于模块目录（默认 `/data/adb/modules/battery_module/config.conf`），缺失的键使用默认值。

| 键 | 默认 | 说明 |
|---|---|---|
| ENABLE_MONITOR | true | 电量监控总开关 |
| POLL_SECS | 10 | sysfs 轮询间隔(秒)；电量下发固定 30s 一次，不受此值影响 |
| ENABLE_TEMP_COMP | true | 禁用温度补偿 |
| V_CURVE | 见下 | 电压→电量分段表，格式 `mV:百分比,mV:百分比,...` |
| R_MOHM | 40 | 电池内阻(mΩ)，放电负载补偿用 |
| MIN_PERCENT | 1 | 显示电量下限 |
| RELAX_AFTER_UNPLUG_SECS | 300 | 拔线后弛豫窗口时长(秒) |
| KERNEL_STUCK_TIMEOUT_SECS | 900 | 内核电量无变化超时(秒) |
| RATE_DISCHARGE_DOWN_SECS | 60 | 放电下降速率 1%/N 秒 |
| RATE_DISCHARGE_UP_SECS | 180 | 放电回升速率 1%/N 秒 |
| RATE_VALVE_SECS | 10 | 安全阀下探速率 1%/N 秒 |
| SHUTDOWN_VALVE_MV | 3130 | 安全阀触发电压（裸端电压），按实测低端取真实 ~1% 处；实测裸电压最低 3122mV |
| VALVE_COMP_MV | 3250 | 补偿后电压低于此值时 target 封顶 |
| VALVE_CAP_PERCENT | 5 | 上述封顶值 |
| CURRENT_SIGN | 0 | 电流符号：0=自动 1=正为充电 -1=正为放电 |
| CALIB_LOG | false | 放电时每拍打点 `v_comp`/`k`，用于拟合 V_CURVE |

默认 V_CURVE 由五份实机放电日志拟合（2134 个打点，RMSE≈0.97%，45 点稀疏化）：3140mV 起全段有实测覆盖，仅 3140mV 以下短尾沿 3050:0 外推，4465mV 为满电锚点：

```
V_CURVE=3050:0,3160:2.1,3270:5.6,3460:11,3620:17.8,3630:19,3660:19.8,3690:22.4,3710:25.9,3720:29.4,3730:30.6,3740:31,3750:32.6,3800:37.7,3810:39.1,3830:40.5,3850:43,3870:44.5,3900:49.5,3910:52.2,3920:53,3930:55,3960:57.2,3970:59.3,4020:63.6,4070:66.3,4080:67.5,4090:67.6,4100:69.5,4130:71.9,4140:72.2,4150:74,4200:78,4210:79.4,4250:81.2,4260:82.7,4270:82.9,4280:84,4290:86,4310:88.4,4350:90.8,4370:94.1,4410:97.3,4460:97.9,4465:100
```

曲线校准：`CALIB_LOG=true` 后完整放电一次，把日志交给 `analysis/fit_curve.py` 重拟合 V_CURVE，再关闭打点。
每次下发电量的日志都会打印 `rm`/`fcc` 绝对值(mAh)，小板谎报容量（如 6200mAh 被报成 4800mAh）可直接从日志看出来。
