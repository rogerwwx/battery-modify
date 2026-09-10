#!/usr/bin/env python3
# fit_curve.py —— 从 battery_calibrate.log 重拟合 V_CURVE（勿再手工特调）
#
# 用法:
#   python fit_curve.py log1.log log2.log ...      # 指定日志
#   python fit_curve.py                            # 默认扫描当前目录 *.log
#
# 流程: 解析 set level 打点 -> 10mV 分箱中位数 -> PAVA 加权保序回归
#       -> 偏差<=TOL 贪心稀疏化 -> 输出可直接粘贴进 config.conf 的 V_CURVE 行
#
# 真值说明: 以 k=RM/FCC 为真值, 仅在 RM 健康的日志上拟合;
# 含大量 [内核不动] 的日志会扭曲结果, 脚本会给出警告。
# 低段: log2_lowtail.log 低电量段已把实测覆盖推进到 3136mV, 故外推尾只剩 3050 显示零点;
# 输出与代码内 DEFAULT_CURVE 的偏差应在 TOL 之内; 贪心稀疏化不唯一, 重跑不保证逐点一致。

import re, sys, glob
from statistics import median

TOL = 0.35          # 稀疏化最大偏差(%)
BIN_MV = 10         # 分箱宽度(mV)
TAIL = [(3050, 0.0)]  # 外推锚: 实测最低打点 3136mV 以下仅接显示零点
TOP = (4465, 100.0)   # 满电锚点

LINE = re.compile(
    r"^\[\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\] set level \d+ \| .*?"
    r"v=([\d.]+)%\(\u8865\u507f(\d+)mV/\u88f8(\d+)mV\) "
    r"k=(?:([\d.]+)|Some\(([\d.]+)\)) rm=(\d+)/fcc=(\d+)mAh(.*)$"
)

def parse(paths):
    rows = []   # (v_comp, k, flags)
    for p in paths:
        stuck = 0
        with open(p, encoding="utf-8", errors="replace") as f:
            for line in f:
                m = LINE.match(line)
                if not m:
                    continue
                v_comp, k = int(m.group(2)), float(m.group(4) or m.group(5))
                flags = m.group(8) or ""
                if "\u5185\u6838\u4e0d\u52a8" in flags:
                    stuck += 1
                rows.append((v_comp, k))
        if stuck > 20:
            print(f"警告: {p} 含 {stuck} 条 [内核不动] 打点, RM 可能不可信, 建议剔除该文件", file=sys.stderr)
    return rows

def pava(pts):
    """加权保序回归, pts=[(mv, weight, y)] 按 mv 升序, 返回 [(mv, weight, y)] 单调不减"""
    st = []
    for mv, w, y in pts:
        st.append([mv, w, y, w, w * y])  # mv, 展示w, y, sumW, sumWY
        while len(st) >= 2 and st[-2][2] > st[-1][2]:
            b, a = st.pop(), st.pop()
            W, WY = a[3] + b[3], a[4] + b[4]
            st.append([b[0], W, WY / W, W, WY])
    return [(int(mv), y) for mv, _w, y, _sw, _swy in st]

def decimate(pts, tol):
    keep, i, n = [0], 0, len(pts)
    while i < n - 1:
        best = i + 1
        for k in range(i + 1, n):
            bad = False
            for m in range(i + 1, k):
                t = (pts[m][0] - pts[i][0]) / (pts[k][0] - pts[i][0])
                if abs(pts[i][1] + t * (pts[k][1] - pts[i][1]) - pts[m][1]) > tol:
                    bad = True
                    break
            if bad:
                break
            best = k
        keep.append(best)
        i = best
    return [pts[k] for k in keep]

def interp(curve, mv):
    if mv <= curve[0][0]:
        return curve[0][1]
    if mv >= curve[-1][0]:
        return curve[-1][1]
    for (a, pa), (b, pb) in zip(curve, curve[1:]):
        if a <= mv <= b:
            return pa + (mv - a) / (b - a) * (pb - pa)
    return curve[-1][1]

def main():
    paths = sys.argv[1:] or sorted(glob.glob("*.log"))
    if not paths:
        sys.exit("未找到日志文件")
    rows = parse(paths)
    print(f"解析打点 {len(rows)} 条 来自 {len(paths)} 个文件", file=sys.stderr)

    bins = {}
    for v_comp, k in rows:
        bins.setdefault(round(v_comp / BIN_MV) * BIN_MV, []).append(k)
    pts = [(mv, len(ks), median(ks)) for mv, ks in sorted(bins.items())]
    iso = pava(pts)

    full = TAIL + [(mv, y) for mv, y in iso if mv > TAIL[-1][0]] + [TOP]
    curve = decimate(full, TOL)

    se = sum((interp(curve, v) - k) ** 2 for v, k in rows)
    ae = sum(abs(interp(curve, v) - k) for v, k in rows)
    print(f"稀疏化后 {len(curve)} 点 | RMSE={(se/len(rows))**0.5:.2f}% MAE={ae/len(rows):.2f}%", file=sys.stderr)
    print("V_CURVE=" + ",".join(f"{mv}:{y:g}" for mv, y in curve))

if __name__ == "__main__":
    main()
