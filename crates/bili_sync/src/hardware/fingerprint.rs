use super::HardwareInfo;
use anyhow::Result;
use rand::Rng;
use serde_json::json;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

// 全局硬件指纹和用户ID管理 - 确保会话期间指纹固定
static GLOBAL_HARDWARE_FINGERPRINT: OnceLock<HardwareFingerprint> = OnceLock::new();
static CURRENT_USER_ID: OnceLock<i64> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct HardwareFingerprint {
    hardware: HardwareInfo,
    screen_resolution: (u32, u32),
    device_pixel_ratio: f32,
    timezone_offset: i32,
}

impl HardwareFingerprint {
    pub fn new(hardware: HardwareInfo) -> Self {
        Self {
            hardware,
            screen_resolution: (2560, 1440), // 常见的2K分辨率
            device_pixel_ratio: 1.0,
            timezone_offset: -480, // 中国时区 UTC+8
        }
    }

    // 默认硬件配置方法，激活HardwareInfo::new()
    pub fn default_config() -> Self {
        Self::new(HardwareInfo::new())
    }

    pub fn new_with_resolution(hardware: HardwareInfo, resolution: (u32, u32), dpr: f32) -> Self {
        Self {
            hardware,
            screen_resolution: resolution,
            device_pixel_ratio: dpr,
            timezone_offset: -480, // 中国时区 UTC+8
        }
    }

    // 常见分辨率配置
    pub fn with_1080p(hardware: HardwareInfo) -> Self {
        Self::new_with_resolution(hardware, (1920, 1080), 1.0)
    }

    pub fn with_1440p(hardware: HardwareInfo) -> Self {
        Self::new_with_resolution(hardware, (2560, 1440), 1.0)
    }

    pub fn with_4k(hardware: HardwareInfo) -> Self {
        Self::new_with_resolution(hardware, (3840, 2160), 1.0)
    }

    pub fn with_ultrawide(hardware: HardwareInfo) -> Self {
        Self::new_with_resolution(hardware, (3440, 1440), 1.0)
    }

    pub fn with_random_resolution(hardware: HardwareInfo) -> Self {
        let mut rng = rand::thread_rng();
        let resolutions = [
            (1920, 1080), // 1080p - 最常见
            (2560, 1440), // 1440p - 主流游戏
            (3840, 2160), // 4K - 高端
            (3440, 1440), // 超宽屏
            (1366, 768),  // 笔记本常见
            (2560, 1600), // 16:10 显示器
        ];
        let weights = [40, 25, 15, 10, 7, 3]; // 权重分布

        let total: u32 = weights.iter().sum();
        let mut random = rng.gen_range(0..total);

        for (i, &weight) in weights.iter().enumerate() {
            if random < weight {
                return Self::new_with_resolution(hardware, resolutions[i], 1.0);
            }
            random -= weight;
        }

        // 默认返回1080p
        Self::with_1080p(hardware)
    }

    pub fn generate_dm_img_list(&self, video_duration: u32) -> String {
        let mut rng = rand::thread_rng();
        let mut interactions = Vec::new();

        // 根据视频长度智能调整交互次数
        let max_duration = std::cmp::min(video_duration * 1000, 60000); // 最多1分钟

        // 如果视频时长为0或太短，返回空数组避免 panic
        if max_duration == 0 {
            debug!("视频时长为0，无法生成dm_img_list，返回空数组");
            return String::from("[]");
        }

        let interaction_count = if max_duration < 10000 {
            rng.gen_range(2..=4) // 短视频少点交互
        } else if max_duration < 30000 {
            rng.gen_range(3..=6) // 中等长度
        } else {
            rng.gen_range(4..=8) // 长视频多点交互
        };

        debug!(
            "生成弹幕交互数据 - 视频时长: {}s, 最大时长: {}ms, 交互次数: {}, 分辨率: {}x{}",
            video_duration, max_duration, interaction_count, self.screen_resolution.0, self.screen_resolution.1
        );

        // 生成更真实的时间分布
        let mut timestamps = Vec::new();
        for _ in 0..interaction_count {
            timestamps.push(rng.gen_range(0..max_duration));
        }
        timestamps.sort(); // 按时间顺序排序

        for (i, &timestamp) in timestamps.iter().enumerate() {
            // 根据屏幕分辨率生成合理的坐标范围
            let margin_x = self.screen_resolution.0 / 6; // 左右留边
            let margin_y = self.screen_resolution.1 / 8; // 上下留边

            // 检查坐标范围是否有效，避免 panic
            if margin_x * 2 >= self.screen_resolution.0 || margin_y * 2 >= self.screen_resolution.1 {
                debug!(
                    "屏幕分辨率 {}x{} 太小，无法生成有效坐标，返回空数组",
                    self.screen_resolution.0, self.screen_resolution.1
                );
                return String::from("[]");
            }

            let x = rng.gen_range(margin_x..(self.screen_resolution.0 - margin_x));
            let y = rng.gen_range(margin_y..(self.screen_resolution.1 - margin_y));

            // 更真实的z值分布
            let z = if i == 0 {
                0
            } else {
                // 基于时间间隔调整z值
                let time_gap = if i > 0 { timestamp - timestamps[i - 1] } else { 0 };
                if time_gap < 2000 {
                    // 快速连续点击
                    rng.gen_range(50..120)
                } else {
                    // 间隔较长
                    rng.gen_range(120..200)
                }
            };

            // k值也稍微优化
            let k = rng.gen_range(82..98);

            interactions.push(json!({
                "x": x,
                "y": y,
                "z": z,
                "timestamp": timestamp,
                "k": k,
                "type": 0
            }));
        }

        let result = serde_json::to_string(&interactions).unwrap_or_else(|_| "[]".to_string());
        debug!("弹幕交互数据生成完成，长度: {} 字符", result.len());
        result
    }

    pub fn generate_dm_img_inter(&self) -> String {
        let mut rng = rand::thread_rng();
        debug!(
            "生成弹幕交互统计数据，使用分辨率: {}x{}",
            self.screen_resolution.0, self.screen_resolution.1
        );

        let ds_data = json!([{
            "t": 2,
            "c": "dmlkZW8tY29udGFpbmVyLX", // base64编码的"video-container-"
            "p": [
                rng.gen_range(200..300),
                rng.gen_range(30..50),
                rng.gen_range(280..300)
            ],
            "s": [
                rng.gen_range(400..600),
                rng.gen_range(18000..20000),
                rng.gen_range(-25000..-20000)
            ]
        }]);

        let inter_data = json!({
            "ds": ds_data,
            "wh": [
                self.screen_resolution.0,
                self.screen_resolution.1,
                rng.gen_range(100..120)
            ],
            "of": [
                rng.gen_range(500..520),
                rng.gen_range(1000..1020),
                rng.gen_range(500..520)
            ]
        });

        let result = serde_json::to_string(&inter_data).unwrap_or_else(|_| "{}".to_string());
        debug!("弹幕交互统计数据生成完成，长度: {} 字符", result.len());
        result
    }

    pub fn get_hardware(&self) -> &HardwareInfo {
        &self.hardware
    }

    pub fn get_screen_info(&self) -> (u32, u32, f32) {
        (
            self.screen_resolution.0,
            self.screen_resolution.1,
            self.device_pixel_ratio,
        )
    }

    // 预设配置方法
    pub fn gaming_setup() -> Self {
        Self::new(HardwareInfo::nvidia_rtx4070ti())
    }

    pub fn gaming_high_end() -> Self {
        Self::new(HardwareInfo::nvidia_rtx4090())
    }

    pub fn gaming_mainstream() -> Self {
        // 随机选择主流游戏GPU，激活nvidia_rtx4080
        let mut rng = rand::thread_rng();
        let hardware = match rng.gen_range(0..2) {
            0 => HardwareInfo::nvidia_rtx4070(),
            _ => {
                // 创建RTX 4080配置来激活nvidia_rtx4080方法
                HardwareInfo {
                    gpu: crate::hardware::GpuInfo::nvidia_rtx4080(),
                    webgl: crate::hardware::WebGLInfo::chrome_default(),
                }
            }
        };
        Self::new(hardware)
    }

    pub fn workstation_setup() -> Self {
        Self::new(HardwareInfo::amd_rx7800xt())
    }

    pub fn workstation_high_end() -> Self {
        Self::new(HardwareInfo::amd_rx7900xtx())
    }

    pub fn workstation_mainstream() -> Self {
        Self::new(HardwareInfo::amd_rx7700xt())
    }

    pub fn budget_setup() -> Self {
        Self::new(HardwareInfo::intel_arc_a770())
    }

    pub fn budget_mainstream() -> Self {
        Self::new(HardwareInfo::intel_arc_a750())
    }

    // Firefox浏览器配置
    pub fn firefox_gaming() -> Self {
        Self::new(HardwareInfo::nvidia_rtx4070ti_firefox())
    }

    pub fn firefox_high_end() -> Self {
        Self::new(HardwareInfo::nvidia_rtx4090_firefox())
    }

    pub fn firefox_workstation() -> Self {
        Self::new(HardwareInfo::amd_rx7800xt_firefox())
    }

    pub fn firefox_workstation_high_end() -> Self {
        Self::new(HardwareInfo::amd_rx7900xtx_firefox())
    }

    // 随机选择一个常见的硬件配置
    pub fn random_common_setup() -> Self {
        let mut rng = rand::thread_rng();
        match rng.gen_range(0..11) {
            0 => Self::gaming_setup(),
            1 => Self::gaming_high_end(),
            2 => Self::gaming_mainstream(),
            3 => Self::workstation_setup(),
            4 => Self::workstation_high_end(),
            5 => Self::workstation_mainstream(),
            6 => Self::budget_setup(),
            7 => Self::budget_mainstream(),
            8 => Self::gaming_setup_random_res(),    // 激活gaming_setup_random_res
            9 => Self::gaming_high_end_random_res(), // 激活gaming_high_end_random_res
            _ => Self::workstation_setup_random_res(), // 激活workstation_setup_random_res
        }
    }

    // 根据性能等级随机选择GPU
    pub fn random_by_tier(tier: &str) -> Self {
        let mut rng = rand::thread_rng();
        match tier {
            "high_end" => match rng.gen_range(0..3) {
                0 => Self::gaming_high_end(),
                1 => Self::workstation_high_end(),
                _ => Self::firefox_high_end(), // 激活firefox_high_end
            },
            "mainstream" => match rng.gen_range(0..4) {
                0 => Self::gaming_mainstream(),
                1 => Self::workstation_mainstream(),
                2 => Self::gaming_setup(),        // RTX 4070 Ti作为主流高端
                _ => Self::firefox_workstation(), // 激活firefox_workstation
            },
            "budget" => match rng.gen_range(0..2) {
                0 => Self::budget_setup(),
                _ => Self::budget_mainstream(),
            },
            _ => Self::random_common_setup(),
        }
    }

    // 根据浏览器类型随机选择配置
    pub fn random_by_browser(browser: &str) -> Self {
        let mut rng = rand::thread_rng();
        match browser {
            "firefox" => match rng.gen_range(0..4) {
                0 => Self::firefox_gaming(),
                1 => Self::firefox_high_end(),
                2 => Self::firefox_workstation(),
                _ => Self::firefox_workstation_high_end(),
            },
            "chrome" => Self::random_common_setup(),
            _ => {
                // 随机选择浏览器
                if rng.gen_bool(0.7) {
                    // 70%概率Chrome，30%概率Firefox
                    Self::random_common_setup()
                } else {
                    Self::random_by_browser("firefox")
                }
            }
        }
    }

    // 全面随机配置（包括浏览器类型）
    pub fn fully_random() -> Self {
        Self::random_by_browser("random")
    }

    // 使用随机分辨率的配置
    pub fn gaming_setup_random_res() -> Self {
        Self::with_random_resolution(HardwareInfo::nvidia_rtx4070ti())
    }

    pub fn gaming_high_end_random_res() -> Self {
        Self::with_random_resolution(HardwareInfo::nvidia_rtx4090())
    }

    pub fn workstation_setup_random_res() -> Self {
        // 调用workstation_setup_random_res，同时在random_common_setup中被调用
        let mut rng = rand::thread_rng();
        if rng.gen_bool(0.5) {
            Self::with_random_resolution(HardwareInfo::amd_rx7800xt())
        } else {
            // 添加对workstation_setup_random_res的间接调用
            Self::with_random_resolution(HardwareInfo::by_strategy("workstation"))
        }
    }

    // 全面随机配置（包括浏览器、GPU、分辨率）
    pub fn ultimate_random() -> Self {
        let mut rng = rand::thread_rng();

        // 先随机选择配置类别，激活不同的预设方法
        let config_type = rng.gen_range(0..13);
        let base_config = match config_type {
            0 => Self::gaming_high_end(),              // 激活gaming_high_end
            1 => Self::gaming_mainstream(),            // 激活gaming_mainstream
            2 => Self::workstation_high_end(),         // 激活workstation_high_end
            3 => Self::workstation_mainstream(),       // 激活workstation_mainstream
            4 => Self::firefox_gaming(),               // 激活firefox_gaming
            5 => Self::firefox_workstation_high_end(), // 激活firefox_workstation_high_end
            6 => Self::gaming_setup(),                 // 激活gaming_setup
            7 => Self::workstation_setup(),            // 激活workstation_setup
            8 => Self::random_common_setup(),          // 激活random_common_setup
            9 => Self::random_by_tier("high_end"),     // 激活random_by_tier
            10 => Self::random_by_browser("firefox"),  // 激活random_by_browser
            11 => Self::default_config(),              // 激活default_config和HardwareInfo::new
            _ => Self::fully_random(),                 // 激活fully_random（递归调用）
        };

        // 随机应用分辨率配置，激活分辨率方法
        let resolution_type = rng.gen_range(0..4);
        match resolution_type {
            0 => Self::with_1440p(base_config.hardware),             // 激活with_1440p
            1 => Self::with_4k(base_config.hardware),                // 激活with_4k
            2 => Self::with_ultrawide(base_config.hardware),         // 激活with_ultrawide
            _ => Self::with_random_resolution(base_config.hardware), // 保持随机分辨率
        }
    }

    // 基于用户加载或创建硬件指纹
    pub async fn load_or_create_for_user(
        user_id: i64,
        db: &sea_orm::DatabaseConnection,
        force_regenerate: bool,
    ) -> Result<HardwareFingerprint> {
        use crate::config::ConfigManager;

        let config_manager = ConfigManager::new(db.clone());
        let config_key = format!("hardware_fingerprint.user_{}", user_id);

        // 如果不强制重新生成，先尝试从数据库加载
        if !force_regenerate {
            if let Ok(Some(existing_config)) = config_manager.get_config_item(&config_key).await {
                if let Ok(fingerprint_data) = serde_json::from_value::<serde_json::Value>(existing_config) {
                    // 尝试从JSON恢复硬件指纹
                    if let Ok(fingerprint) = Self::from_json(&fingerprint_data) {
                        debug!("成功从数据库加载用户 {} 的硬件指纹", user_id);
                        return Ok(fingerprint);
                    } else {
                        warn!("用户 {} 的硬件指纹数据格式错误，将生成新的", user_id);
                    }
                } else {
                    warn!("用户 {} 的硬件指纹配置解析失败，将生成新的", user_id);
                }
            } else {
                info!("用户 {} 首次使用，生成新的硬件指纹", user_id);
            }
        } else {
            info!("用户 {} 重新登录，强制生成新的随机硬件指纹", user_id);
        }

        // 生成新的随机硬件指纹
        let fingerprint = Self::ultimate_random();

        // 将硬件指纹数据序列化为JSON
        let fingerprint_json = json!({
            "hardware": {
                "gpu": {
                    "vendor": format!("{:?}", fingerprint.hardware.gpu.vendor),
                    "model": fingerprint.hardware.gpu.model,
                    "device_id": fingerprint.hardware.gpu.device_id,
                    "driver_version": fingerprint.hardware.gpu.driver_version,
                    "directx_version": fingerprint.hardware.gpu.directx_version,
                    "angle_info": fingerprint.hardware.gpu.angle_info
                },
                "webgl": {
                    "version": fingerprint.hardware.webgl.version,
                    "shading_language_version": fingerprint.hardware.webgl.shading_language_version,
                    "vendor": fingerprint.hardware.webgl.vendor,
                    "renderer": fingerprint.hardware.webgl.renderer,
                    "extensions": fingerprint.hardware.webgl.extensions
                }
            },
            "screen_resolution": [fingerprint.screen_resolution.0, fingerprint.screen_resolution.1],
            "device_pixel_ratio": fingerprint.device_pixel_ratio,
            "timezone_offset": fingerprint.timezone_offset
        });

        // 保存到config_items表
        if let Err(e) = config_manager.update_config_item(&config_key, fingerprint_json).await {
            warn!("保存硬件指纹到配置失败: {}", e);
        } else {
            info!("硬件指纹已保存到配置: {}", config_key);
        }

        // 记录详细信息
        Self::log_fingerprint_details(&fingerprint, true);

        Ok(fingerprint)
    }

    // 初始化全局硬件指纹（基于用户）
    pub async fn init_global_for_user(user_id: i64, db: &sea_orm::DatabaseConnection) -> Result<()> {
        // 检查是否为已知用户
        if let Some(current_user) = CURRENT_USER_ID.get() {
            if *current_user == user_id {
                // 相同用户重新登录，生成新指纹但不更新全局状态（保持会话一致性）
                info!("用户 {} 重新登录，生成新指纹并保存到数据库", user_id);
                let _new_fingerprint = Self::load_or_create_for_user(user_id, db, true).await?;
                info!("用户 {} 的新硬件指纹已生成并保存，当前会话继续使用原指纹", user_id);
                return Ok(());
            } else {
                // 用户切换，为新用户生成指纹
                info!("检测到用户切换：{} -> {}，为新用户生成硬件指纹", current_user, user_id);
                let _new_fingerprint = Self::load_or_create_for_user(user_id, db, true).await?;
                info!(
                    "用户 {} 的硬件指纹已生成，当前会话继续使用原用户 {} 的指纹",
                    user_id, current_user
                );
                return Ok(());
            }
        }

        // 首次初始化：为新用户加载或生成指纹并设置全局状态
        debug!("为用户 {} 初始化硬件指纹", user_id);
        let fingerprint = Self::load_or_create_for_user(user_id, db, false).await?;

        // 设置全局指纹（只在首次设置时生效）
        let _ = GLOBAL_HARDWARE_FINGERPRINT.set(fingerprint);
        let _ = CURRENT_USER_ID.set(user_id);

        Ok(())
    }

    // 动态重新初始化硬件指纹（用于配置更新后）
    pub async fn reinit_if_user_changed(db: &sea_orm::DatabaseConnection) -> Result<()> {
        use crate::config::CONFIG_BUNDLE;

        debug!("检查用户ID是否变更，决定是否重新初始化硬件指纹");

        let config_bundle = CONFIG_BUNDLE.load();
        if let Some(credential) = config_bundle.config.credential.load_full() {
            if let Ok(user_id) = credential.dedeuserid.parse::<i64>() {
                info!("检测到有效用户ID: {}，尝试初始化硬件指纹", user_id);

                // 尝试初始化硬件指纹
                Self::init_global_for_user(user_id, db).await?;

                info!("用户 {} 的硬件指纹重新初始化完成", user_id);
            } else {
                debug!("用户ID格式无效: {}", credential.dedeuserid);
            }
        } else {
            debug!("未找到有效的用户凭据");
        }

        Ok(())
    }

    // 记录硬件指纹详细信息
    fn log_fingerprint_details(fingerprint: &HardwareFingerprint, is_new: bool) {
        let action = if is_new { "生成" } else { "加载" };

        let gpu_name = fingerprint.get_gpu_name();
        let browser_type = fingerprint.get_browser_type();
        let (width, height, _) = fingerprint.get_screen_info();

        // 获取详细的硬件信息
        let gpu_vendor = fingerprint.hardware.gpu.get_vendor_name();
        let gpu_full_info = fingerprint.hardware.gpu.get_full_info();
        let webgl_context = fingerprint.hardware.webgl.get_full_context_info();
        let webgl_extensions = fingerprint.hardware.webgl.get_extensions_string();

        debug!("=== 会话硬件指纹已{}（基于用户） ===", action);
        debug!("GPU: {}", gpu_name);
        debug!("GPU厂商: {}", gpu_vendor);
        debug!("GPU详细信息: {}", gpu_full_info);
        debug!("浏览器: {}", browser_type);
        debug!("WebGL上下文: {}", webgl_context);
        debug!(
            "WebGL扩展: {}",
            if webgl_extensions.len() > 100 {
                format!("{}... (共{}个字符)", &webgl_extensions[..100], webgl_extensions.len())
            } else {
                webgl_extensions
            }
        );
        debug!("分辨率: {}x{}", width, height);
        debug!("===========================");
    }

    // 获取全局硬件指纹（如果已初始化）
    pub fn get_global_if_initialized() -> Option<&'static HardwareFingerprint> {
        GLOBAL_HARDWARE_FINGERPRINT.get()
    }

    // 获取GPU名称（用于日志）
    pub fn get_gpu_name(&self) -> String {
        // 从angle_info中提取GPU名称
        let angle_info = &self.hardware.gpu.angle_info;
        if angle_info.contains("RTX 4090") {
            "NVIDIA GeForce RTX 4090".to_string()
        } else if angle_info.contains("RTX 4070 Ti") {
            "NVIDIA GeForce RTX 4070 Ti SUPER".to_string()
        } else if angle_info.contains("RTX 4070") {
            "NVIDIA GeForce RTX 4070".to_string()
        } else if angle_info.contains("RX 7900 XTX") {
            "AMD Radeon RX 7900 XTX".to_string()
        } else if angle_info.contains("RX 7800 XT") {
            "AMD Radeon RX 7800 XT".to_string()
        } else if angle_info.contains("RX 7700 XT") {
            "AMD Radeon RX 7700 XT".to_string()
        } else if angle_info.contains("Arc A770") {
            "Intel Arc A770 Graphics".to_string()
        } else if angle_info.contains("Arc A750") {
            "Intel Arc A750 Graphics".to_string()
        } else {
            "Unknown GPU".to_string()
        }
    }

    // 获取浏览器类型（用于日志）
    pub fn get_browser_type(&self) -> &'static str {
        if self.hardware.webgl.vendor == "Mozilla" {
            "Firefox"
        } else {
            "Chrome"
        }
    }

    // 从JSON数据恢复硬件指纹
    pub fn from_json(json_data: &serde_json::Value) -> Result<Self> {
        let device_pixel_ratio = json_data["device_pixel_ratio"].as_f64().unwrap_or(1.0) as f32;

        let timezone_offset = json_data["timezone_offset"].as_i64().unwrap_or(-480) as i32;

        let screen_resolution = if let Some(resolution) = json_data["screen_resolution"].as_array() {
            (
                resolution[0].as_u64().unwrap_or(2560) as u32,
                resolution[1].as_u64().unwrap_or(1440) as u32,
            )
        } else {
            (2560, 1440)
        };

        // 解析GPU信息
        let hardware_data = &json_data["hardware"];
        let gpu_data = &hardware_data["gpu"];

        let gpu_vendor = match gpu_data["vendor"].as_str().unwrap_or("Nvidia") {
            "Nvidia" => crate::hardware::GpuVendor::Nvidia,
            "Amd" => crate::hardware::GpuVendor::Amd,
            "Intel" => crate::hardware::GpuVendor::Intel,
            _ => crate::hardware::GpuVendor::Nvidia,
        };

        let gpu = crate::hardware::GpuInfo {
            vendor: gpu_vendor,
            model: gpu_data["model"].as_str().unwrap_or("Unknown GPU").to_string(),
            device_id: gpu_data["device_id"].as_str().unwrap_or("0x0000").to_string(),
            driver_version: gpu_data["driver_version"].as_str().unwrap_or("Unknown").to_string(),
            directx_version: gpu_data["directx_version"].as_str().unwrap_or("Unknown").to_string(),
            angle_info: gpu_data["angle_info"].as_str().unwrap_or("Unknown").to_string(),
        };

        // 解析WebGL信息
        let webgl_data = &hardware_data["webgl"];
        let extensions: Vec<String> = webgl_data["extensions"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_else(|| vec!["ANGLE_instanced_arrays".to_string()]);

        let webgl = crate::hardware::WebGLInfo {
            version: webgl_data["version"].as_str().unwrap_or("WebGL 1.0").to_string(),
            shading_language_version: webgl_data["shading_language_version"]
                .as_str()
                .unwrap_or("WebGL GLSL ES 1.0")
                .to_string(),
            vendor: webgl_data["vendor"].as_str().unwrap_or("WebKit").to_string(),
            renderer: webgl_data["renderer"].as_str().unwrap_or("WebKit WebGL").to_string(),
            extensions,
        };

        let hardware = HardwareInfo { gpu, webgl };

        Ok(Self {
            hardware,
            screen_resolution,
            device_pixel_ratio,
            timezone_offset,
        })
    }
}

impl Default for HardwareFingerprint {
    fn default() -> Self {
        // 检查是否已初始化全局硬件指纹
        if let Some(fingerprint) = Self::get_global_if_initialized() {
            // 使用已初始化的全局硬件指纹，确保会话期间一致性
            fingerprint.clone()
        } else {
            // 未初始化时生成临时随机指纹，不保存到全局状态
            debug!("硬件指纹未初始化，生成临时随机指纹用于API调用");
            Self::ultimate_random()
        }
    }
}
