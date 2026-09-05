# battery-modify

Android (Magisk 模块) 电量接管守护进程：用电池电压模拟电量，融合内核 fuel gauge 的 RM/FCC，
经平滑限速后通过 `dumpsys battery set level` 下发，避免小板错误数据导致的电量跳变。

## 工作原理

- 按 `POLL_SECS`（默认 10s）轮询读取 `voltage_now` / `current_now` / `status` / RM / FCC（节点按实机写死：`/sys/class/power_supply/battery/` 下的 `voltage_now`/`current_now`/`charge_counter`/`charge_full`）
- 放电时按内阻做负载补偿（`v + |I|×R`），查分段线性表得 voltage_percent，经中位数+EMA 降噪
- kernel_percent = RM × 100 / FCC，带毛刺拒绝（单拍跳变 >8% 需下一拍确认）
- 放电：`target = max(voltage_percent, kernel_percent)`；内核电量长期不动或高出电压过多时回落到纯电压
- 充电：以 kernel 为主，smooth 只升不降；内核卡死超时后改由电压死推但封顶（防 CV 阶段虚高冲 100）
- 拔线后 5 分钟弛豫窗口内直接跟随 kernel（吸收表面电荷导致的电压虚高）
- smooth_percent 按时间基限速逼近 target（各方向速率独立可配），每 30s 下发一次（与旧版频率一致）
- smooth / voltage / kernel 均到顶（或系统报 Full）连续确认 3 拍后 `dumpsys battery reset` 退出接管
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
| CHARGE_V_CAP | 96 | 充电电压兜底路径的封顶百分比 |
| RELAX_AFTER_UNPLUG_SECS | 300 | 拔线后弛豫窗口时长(秒) |
| KERNEL_STUCK_TIMEOUT_SECS | 900 | 内核电量无变化超时(秒) |
| RATE_DISCHARGE_DOWN_SECS | 120 | 放电下降速率 1%/N 秒 |
| RATE_DISCHARGE_UP_SECS | 180 | 放电回升速率 1%/N 秒 |
| RATE_CHARGE_UP_SECS | 45 | 充电上升速率 1%/N 秒 |
| RATE_CHARGE_STUCK_SECS | 300 | 内核卡死时死推速率 1%/N 秒 |
| RATE_VALVE_SECS | 10 | 安全阀下探速率 1%/N 秒 |
| SHUTDOWN_VALVE_MV | 3150 | 安全阀触发电压（裸端电压） |
| VALVE_COMP_MV | 3250 | 补偿后电压低于此值时 target 封顶 |
| VALVE_CAP_PERCENT | 5 | 上述封顶值 |
| CURRENT_SIGN | 0 | 电流符号：0=自动 1=正为充电 -1=正为放电 |
| CALIB_LOG | false | 放电时每拍打点 `v_comp`/`k`，用于拟合 V_CURVE |

默认 V_CURVE 按 3.00V~4.45V 体系的锂电池典型 OCV 给出，中段已按实机锚点（补偿后 3.933V ≈ 55%）校准：

```
V_CURVE=3050:0,3180:1,3350:3,3470:5,3620:10,3700:15,3760:20,3810:30,3855:40,3910:50,3960:60,4000:68,4040:75,4090:82,4150:88,4200:92,4250:95,4300:97,4350:98,4400:99,4450:100
```

曲线校准：`CALIB_LOG=true` 后完整放电一次，用日志里 `v_comp` 与 `k` 的对应关系替换 V_CURVE，再关闭打点。
