use crate::source::Reading;
use crate::util::write_log;

/// 电量估计器：库仑计步进 + OCV 锚定 + 内核绝对值融合（互补滤波结构）
///
/// 三路信息源互补特性：
/// - ΔRM/FCC 库仑计步进：短期精确、无延迟，但 RM 卡死/缺席时失效
/// - OCV 查表(v_pct)：无累积漂移，但负载噪声大、弛豫期偏高 → 长时常数慢速锚定
/// - k = RM/FCC 绝对值：电量计健康时最准 → 中时常数融合，同时负责跳变重对齐
///
/// 单拍开销：几次浮点运算，无任何堆分配。
pub struct Estimator {
    /// 融合后的电量估计(%)
    pub soc: f64,
    rm_prev: Option<f64>,
}

/// 单拍库仑计步进上限(%)，超过视为 RM 重置/充电跳变，不做步进
const CC_STEP_MAX: f64 = 3.0;
/// 弛豫窗口内 OCV 锚定强度衰减倍数（端电压被表面电荷抬高，可信度下降）
const RELAX_TAU_MUL: f64 = 4.0;

impl Estimator {
    /// 以接管起点（平滑值）播种；rm_prev 留空，首拍只记基准不产生步进
    pub fn new(seed: f64) -> Estimator {
        Estimator {
            soc: seed,
            rm_prev: None,
        }
    }

    /// 放电模式每拍调用一次，返回融合后的电量估计。
    /// k_fuse 为已通过健康检查（未卡死、与电压偏差不离谱）的内核电量，None = 不参与。
    pub fn update(
        &mut self,
        rd: &Reading,
        v_pct: f64,
        k_fuse: Option<f64>,
        in_relax: bool,
        dt: f64,
        tau_anchor_secs: u64,
        tau_kernel_secs: u64,
    ) -> f64 {
        // ---- 1) 库仑计步进：soc += ΔRM/FCC*100 ----
        if let (Some(rm), Some(fcc)) = (rd.rm_mah, rd.fcc_mah) {
            if fcc > 0.0 {
                if let Some(rp) = self.rm_prev {
                    let step = (rm - rp) * 100.0 / fcc;
                    if step.abs() <= CC_STEP_MAX {
                        self.soc += step;
                    } else if let Some(k) = k_fuse {
                        // RM 大幅跳变（充满复位/重启）：以内核绝对值重新对齐
                        write_log(&format!(
                            "RM 跳变 {:+.1}%（{:.0}→{:.0}mAh），soc 对齐内核 {:.1}%",
                            step, rp, rm, k
                        ));
                        self.soc = k;
                    }
                    // 跳变且无内核参考：丢弃本拍步进，仅更新基准
                }
                self.rm_prev = Some(rm);
            }
        }

        // ---- 2) OCV 锚定：长时常数把估计缓速拉向电压查表值（消除库仑计漂移）----
        let tau = if in_relax {
            tau_anchor_secs as f64 * RELAX_TAU_MUL
        } else {
            tau_anchor_secs as f64
        };
        let a = (dt / tau).min(0.3);
        self.soc += (v_pct - self.soc) * a;

        // ---- 3) 内核绝对值融合：电量计健康时最准，中时常数 ----
        if let Some(k) = k_fuse {
            let ak = (dt / tau_kernel_secs as f64).min(0.5);
            self.soc += (k - self.soc) * ak;
        }

        self.soc
    }
}
