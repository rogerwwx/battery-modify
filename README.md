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
| SHUTDOWN_VALVE_MV | 3150 | 安全阀触发电压（裸端电压） |
| VALVE_COMP_MV | 3250 | 补偿后电压低于此值时 target 封顶 |
| VALVE_CAP_PERCENT | 5 | 上述封顶值 |
| CURRENT_SIGN | 0 | 电流符号：0=自动 1=正为充电 -1=正为放电 |
| CALIB_LOG | false | 放电时每拍打点 `v_comp`/`k`，用于拟合 V_CURVE |

默认 V_CURVE 的 3.38~3.93V 区段按两轮实机放电日志拟合（低段与中段均有实测锚点），顶部以 4.45V 满充为锚，仅 3.38V 以下短尾为外推：

```
V_CURVE=3050:0,3150:3,3250:5,3350:7.5,3400:9,3430:10,3451:11,3475:11.5,3489:12.5,3513:13.5,3540:14.5,3580:16,3605:17.5,3620:18,3640:19,3655:20,3670:21,3680:22,3690:23,3700:24,3710:25,3740:29,3776:34,3800:36,3843:40,3872:45,3904:50,3933:55,3960:60,4000:66,4040:73,4090:80,4150:86,4200:90,4250:94,4300:96,4350:98,4400:99,4450:100
```

曲线校准：`CALIB_LOG=true` 后完整放电一次，用日志里 `v_comp` 与 `k` 的对应关系替换 V_CURVE，再关闭打点。
每次下发电量的日志都会打印 `rm`/`fcc` 绝对值(mAh)，小板谎报容量（如 6200mAh 被报成 4800mAh）可直接从日志看出来。
