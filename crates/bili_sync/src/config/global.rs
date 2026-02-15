use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use arc_swap::ArcSwap;
use clap::Parser;
use once_cell::sync::Lazy;
use tracing::{info, warn};

use crate::config::clap::Args;
use crate::config::{Config, ConfigBundle};

/// 全局的配置包，使用 ArcSwap 支持热重载
/// 包含配置、模板引擎、限流器等所有需要热重载的组件
/// 初始化时使用空配置包，在数据库初始化后再设置真实配置
pub static CONFIG_BUNDLE: Lazy<ArcSwap<ConfigBundle>> =
    Lazy::new(|| ArcSwap::from_pointee(load_minimal_config_bundle()));

/// 全局的配置管理器，用于数据库操作
static CONFIG_MANAGER: Lazy<RwLock<Option<crate::config::ConfigManager>>> = Lazy::new(|| RwLock::new(None));

/// 设置配置管理器（在应用启动时调用）
pub fn set_config_manager(manager: crate::config::ConfigManager) {
    let mut guard = CONFIG_MANAGER.write().unwrap();
    *guard = Some(manager);
    debug!("配置管理器已设置");
}

/// 获取配置管理器（用于credential刷新等场景）
pub fn get_config_manager() -> Option<crate::config::ConfigManager> {
    let guard = CONFIG_MANAGER.read().unwrap();
    guard.clone()
}

/// 重新加载配置包（支持热重载）
pub async fn reload_config_bundle() -> Result<()> {
    debug!("开始重新加载配置包...");

    let manager_opt = {
        let manager_guard = CONFIG_MANAGER.read().unwrap();
        manager_guard.clone()
    };

    let new_bundle = if let Some(manager) = manager_opt {
        // 从数据库加载配置
        debug!("从数据库加载配置");
        manager.load_config_bundle().await?
    } else {
        // 回退到TOML加载
        warn!("配置管理器未初始化，回退到TOML加载");
        let config = load_config();
        ConfigBundle::from_config(config)?
    };

    // 验证新模板是否正确注册
    verify_template_registration(&new_bundle)?;

    // 检查模板是否有变化，这有助于识别可能影响现有视频的更改
    let template_changed = check_template_changes(&new_bundle);
    if template_changed {
        warn!("检测到模板配置变化，现有视频重新处理时将从原始路径重新计算");
    }

    // 原子性更新配置包
    CONFIG_BUNDLE.store(Arc::new(new_bundle));
    debug!("配置包已重新加载并验证");
    Ok(())
}

/// 验证模板注册是否正确
fn verify_template_registration(bundle: &ConfigBundle) -> Result<()> {
    use serde_json::json;

    debug!("验证模板注册...");

    // 测试数据
    let test_data = json!({
        "upper_name": "测试UP主",
        "title": "测试视频标题"
    });

    // 验证video模板
    match bundle.render_video_template(&test_data) {
        Ok(rendered) => {
            debug!("video模板验证成功: '{}'", rendered);

            // 检查路径分隔符处理是否正确
            if bundle.config.video_name.contains('/') && !rendered.contains('/') && !rendered.contains("__UNIX_SEP__") {
                warn!("警告: video模板包含路径分隔符但渲染结果中未找到分隔符: '{}'", rendered);
            }
        }
        Err(e) => {
            error!("video模板验证失败: {}", e);
            return Err(anyhow::anyhow!("video模板验证失败: {}", e));
        }
    }

    // 验证page模板
    match bundle.render_page_template(&test_data) {
        Ok(rendered) => {
            debug!("page模板验证成功: '{}'", rendered);
        }
        Err(e) => {
            error!("page模板验证失败: {}", e);
            return Err(anyhow::anyhow!("page模板验证失败: {}", e));
        }
    }

    debug!("所有模板验证通过");
    Ok(())
}

/// 检查模板是否有变化
fn check_template_changes(new_bundle: &ConfigBundle) -> bool {
    use tracing::debug;

    // 获取当前配置
    let current_bundle = CONFIG_BUNDLE.load();

    // 比较关键模板配置
    let video_name_changed = current_bundle.config.video_name != new_bundle.config.video_name;
    let page_name_changed = current_bundle.config.page_name != new_bundle.config.page_name;
    let multi_page_name_changed = current_bundle.config.multi_page_name != new_bundle.config.multi_page_name;
    let bangumi_name_changed = current_bundle.config.bangumi_name != new_bundle.config.bangumi_name;
    let folder_structure_changed = current_bundle.config.folder_structure != new_bundle.config.folder_structure;
    let bangumi_folder_changed = current_bundle.config.bangumi_folder_name != new_bundle.config.bangumi_folder_name;

    if video_name_changed {
        warn!(
            "视频文件名模板已变更: '{}' -> '{}'",
            current_bundle.config.video_name, new_bundle.config.video_name
        );
    }
    if page_name_changed {
        debug!(
            "分页文件名模板已变更: '{}' -> '{}'",
            current_bundle.config.page_name, new_bundle.config.page_name
        );
    }
    if multi_page_name_changed {
        debug!(
            "多P视频分页模板已变更: '{}' -> '{}'",
            current_bundle.config.multi_page_name, new_bundle.config.multi_page_name
        );
    }
    if bangumi_name_changed {
        debug!(
            "番剧文件名模板已变更: '{}' -> '{}'",
            current_bundle.config.bangumi_name, new_bundle.config.bangumi_name
        );
    }
    if folder_structure_changed {
        debug!(
            "文件夹结构模板已变更: '{}' -> '{}'",
            current_bundle.config.folder_structure, new_bundle.config.folder_structure
        );
    }
    if bangumi_folder_changed {
        debug!(
            "番剧文件夹模板已变更: '{}' -> '{}'",
            current_bundle.config.bangumi_folder_name, new_bundle.config.bangumi_folder_name
        );
    }

    video_name_changed
        || page_name_changed
        || multi_page_name_changed
        || bangumi_name_changed
        || folder_structure_changed
        || bangumi_folder_changed
}

/// 访问配置包的便捷函数
pub fn with_config<F, R>(f: F) -> R
where
    F: FnOnce(&ConfigBundle) -> R,
{
    let bundle = CONFIG_BUNDLE.load();
    f(&bundle)
}

/// 获取配置的便捷函数（向后兼容）
#[allow(dead_code)]
pub fn get_config() -> Arc<ConfigBundle> {
    CONFIG_BUNDLE.load_full()
}

/// 向后兼容的配置重载函数（同步版本）
pub fn reload_config() -> Config {
    // 从当前配置包中提取配置
    with_config(|bundle| bundle.config.clone())
}

/// 向后兼容的全局配置获取函数
#[allow(dead_code)]
pub fn get_current_config() -> Config {
    with_config(|bundle| bundle.config.clone())
}

/// 向后兼容的全局配置引用 - 已弃用，请使用reload_config()函数
#[deprecated(note = "配置现在完全基于数据库，请使用reload_config()函数")]
#[allow(dead_code)]
pub static CONFIG: Lazy<Config> = Lazy::new(load_config);

/// 向后兼容的全局模板引擎引用 - 已弃用，请使用ConfigBundle中的handlebars
#[deprecated(note = "模板引擎现在通过ConfigBundle提供热更新支持，请使用with_config(|bundle| bundle.handlebars)")]
#[allow(dead_code)]
pub static TEMPLATE: Lazy<handlebars::Handlebars<'static>> = Lazy::new(|| {
    use crate::config::PathSafeTemplate;
    use handlebars::handlebars_helper;

    let config = load_config();
    let mut handlebars = handlebars::Handlebars::new();

    // 注册自定义 helper
    handlebars_helper!(truncate: |s: String, len: usize| {
        if s.chars().count() > len {
            s.chars().take(len).collect::<String>()
        } else {
            s.to_string()
        }
    });
    handlebars.register_helper("truncate", Box::new(truncate));

    // 注册所有必需的模板
    let video_name = Box::leak(config.video_name.to_string().into_boxed_str());
    let page_name = Box::leak(config.page_name.to_string().into_boxed_str());
    let multi_page_name = Box::leak(config.multi_page_name.to_string().into_boxed_str());
    let bangumi_name = Box::leak(config.bangumi_name.to_string().into_boxed_str());

    handlebars
        .path_safe_register("video", video_name)
        .expect("注册video模板失败");
    handlebars
        .path_safe_register("page", page_name)
        .expect("注册page模板失败");
    handlebars
        .path_safe_register("multi_page", multi_page_name)
        .expect("注册multi_page模板失败");
    handlebars
        .path_safe_register("bangumi", bangumi_name)
        .expect("注册bangumi模板失败");

    handlebars
});

/// 加载最小配置包（不进行配置检查，避免重复警告）
fn load_minimal_config_bundle() -> ConfigBundle {
    info!("开始加载配置包..");

    // 创建默认配置但不进行检查
    let config = Config::default();
    let bundle = ConfigBundle::from_config(config).expect("创建配置包失败");
    info!("配置包加载完毕");
    bundle
}

/// 加载初始配置包（已弃用，由数据库配置系统取代）
#[allow(dead_code)]
fn load_initial_config_bundle() -> ConfigBundle {
    info!("开始加载配置包..");

    // 初始加载时，配置管理器可能还没有设置，所以先从TOML加载
    let config = load_config();
    let bundle = ConfigBundle::from_config(config).expect("创建配置包失败");
    info!("配置包加载完毕");
    bundle
}

/// 异步初始化配置系统（在数据库连接建立后调用）
pub async fn init_config_with_database(db: sea_orm::DatabaseConnection) -> Result<()> {
    info!("开始初始化数据库配置系统");

    // 创建配置管理器
    let manager = crate::config::ConfigManager::new(db);

    // 确保配置表存在
    manager.ensure_tables_exist().await?;

    // 执行配置结构迁移（标准接口）
    match manager.get_config_schema_status().await {
        Ok(status) => {
            if status.pending {
                info!(
                    "检测到配置迁移待处理: 当前版本={}, 最新版本={}, legacy_detected={}",
                    status.current_version, status.latest_version, status.legacy_detected
                );
            }
        }
        Err(e) => warn!("获取配置迁移状态失败: {}", e),
    }
    if let Err(e) = manager.migrate_config_schema(false).await {
        warn!("配置结构迁移失败（将继续尝试加载配置）: {}", e);
    }

    // 尝试从数据库加载配置，如果失败则从TOML迁移
    let new_bundle = manager.load_config_bundle().await?;

    // 设置全局配置管理器
    set_config_manager(manager);

    // 更新全局配置包
    CONFIG_BUNDLE.store(Arc::new(new_bundle));

    // 配置检查已简化，因为配置现在完全基于数据库
    info!("检查配置..");
    #[cfg(not(test))]
    {
        let config = reload_config();
        if config.check() {
            info!("配置检查通过");
        } else {
            info!("您可以访问管理页 http://{}/ 添加视频源", config.bind_address);
        }
    }
    #[cfg(test)]
    {
        info!("配置检查通过（测试模式）");
    }

    info!("数据库配置系统初始化完成");
    Ok(())
}

/// 向后兼容的配置加载函数
pub fn load_config() -> Config {
    #[cfg(not(test))]
    {
        load_config_impl()
    }
    #[cfg(test)]
    {
        load_config_test()
    }
}

/// 全局的 ARGS，用来解析命令行参数
pub static ARGS: Lazy<Args> = Lazy::new(Args::parse);

/// 全局的 CONFIG_DIR，表示配置文件夹的路径
pub static CONFIG_DIR: Lazy<PathBuf> =
    Lazy::new(|| dirs::config_dir().expect("No config path found").join("bili-sync"));

#[cfg(not(test))]
fn load_config_impl() -> Config {
    info!("开始加载默认配置..");
    // 配置现在完全基于数据库，不再从配置文件加载
    let config = Config::default();
    info!("默认配置加载完毕");
    // 移除配置检查，避免在静态初始化时产生警告
    // 配置检查将在数据库配置系统初始化后进行
    // info!("检查配置..");
    // if config.check() {
    //     info!("配置检查通过");
    // } else {
    //     info!("您可以访问管理页 http://{}/ 添加视频源", config.bind_address);
    // }
    config
}

#[cfg(test)]
fn load_config_test() -> Config {
    let credential = match (
        std::env::var("TEST_SESSDATA"),
        std::env::var("TEST_BILI_JCT"),
        std::env::var("TEST_BUVID3"),
        std::env::var("TEST_DEDEUSERID"),
        std::env::var("TEST_AC_TIME_VALUE"),
    ) {
        (Ok(sessdata), Ok(bili_jct), Ok(buvid3), Ok(dedeuserid), Ok(ac_time_value)) => {
            Some(std::sync::Arc::new(crate::bilibili::Credential {
                sessdata,
                bili_jct,
                buvid3: buvid3.clone(),
                dedeuserid,
                ac_time_value,
                buvid4: Some(buvid3),
                dedeuserid_ckmd5: None,
            }))
        }
        _ => None,
    };
    Config {
        credential: arc_swap::ArcSwapOption::from(credential),
        cdn_sorting: true,
        ..Default::default()
    }
}
