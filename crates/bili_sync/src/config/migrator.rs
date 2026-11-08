use std::path::Path;

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;
use tracing::{info, warn};

use crate::config::{Config, ConfigManager};

/// TOML配置迁移器
pub struct ConfigMigrator {
    manager: ConfigManager,
}

impl ConfigMigrator {
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            manager: ConfigManager::new(db),
        }
    }

    /// 检查是否需要迁移
    pub async fn needs_migration(&self) -> Result<bool> {
        // 检查数据库中是否已有配置
        match self.manager.load_config_bundle().await {
            Ok(_) => {
                info!("数据库中已有配置，无需迁移");
                Ok(false)
            }
            Err(_) => {
                info!("数据库中无配置，需要从TOML迁移");
                Ok(true)
            }
        }
    }

    /// 从TOML文件迁移配置到数据库
    pub async fn migrate_from_toml(&self) -> Result<()> {
        info!("开始从TOML配置迁移到数据库");

        // 加载TOML配置
        let config = self.load_toml_config()?;

        // 保存到数据库
        self.manager
            .save_config(&config)
            .await
            .context("将配置保存到数据库失败")?;

        info!("TOML配置迁移到数据库完成");
        Ok(())
    }

    /// 从数据库导出配置到TOML文件
    pub async fn export_to_toml(&self, output_path: &Path) -> Result<()> {
        info!("开始从数据库导出配置到TOML文件");

        // 从数据库加载配置
        let bundle = self
            .manager
            .load_config_bundle()
            .await
            .context("从数据库加载配置失败")?;

        // 保存为TOML文件
        bundle
            .config
            .save_to_path(output_path)
            .context("保存TOML配置文件失败")?;

        info!("数据库配置导出到TOML文件完成: {}", output_path.display());
        Ok(())
    }

    /// 备份当前TOML配置
    pub fn backup_toml_config(&self) -> Result<()> {
        use crate::config::CONFIG_DIR;

        let config_path = CONFIG_DIR.join("config.toml");
        if !config_path.exists() {
            info!("TOML配置文件不存在，无需备份");
            return Ok(());
        }

        let backup_path = CONFIG_DIR.join(format!(
            "config.toml.backup.{}",
            chrono::Utc::now().format("%Y%m%d_%H%M%S")
        ));

        std::fs::copy(&config_path, &backup_path).with_context(|| {
            format!(
                "备份TOML配置失败: {} -> {}",
                config_path.display(),
                backup_path.display()
            )
        })?;

        info!("TOML配置已备份到: {}", backup_path.display());
        Ok(())
    }

    /// 自动迁移流程
    pub async fn auto_migrate(&self) -> Result<()> {
        // 检查是否需要迁移
        if !self.needs_migration().await? {
            return Ok(());
        }

        info!("检测到需要配置迁移，开始自动迁移流程");

        // 备份现有TOML配置
        if let Err(e) = self.backup_toml_config() {
            warn!("备份TOML配置失败，但继续迁移: {}", e);
        }

        // 执行迁移
        self.migrate_from_toml().await?;

        info!("自动配置迁移完成");
        Ok(())
    }

    /// 验证迁移结果
    pub async fn validate_migration(&self) -> Result<bool> {
        info!("验证配置迁移结果");

        // 尝试加载数据库配置
        let bundle = self
            .manager
            .load_config_bundle()
            .await
            .context("从数据库加载配置失败")?;

        // 验证配置有效性
        let is_valid = bundle.validate();

        if is_valid {
            info!("配置迁移验证通过");
        } else {
            warn!("配置迁移验证失败");
        }

        Ok(is_valid)
    }

    /// 加载TOML配置
    fn load_toml_config(&self) -> Result<Config> {
        // 尝试从文件加载
        match Config::load() {
            Ok(config) => {
                info!("从TOML文件加载配置成功");
                Ok(config)
            }
            Err(e) => {
                warn!("从TOML文件加载配置失败: {}, 使用默认配置", e);
                Ok(Config::default())
            }
        }
    }

    /// 从JSON数据导入配置
    pub async fn import_from_json(&self, json_data: &str) -> Result<()> {
        info!("开始从JSON数据导入配置");

        let config: Config = serde_json::from_str(json_data).context("解析JSON配置数据失败")?;

        self.manager
            .save_config(&config)
            .await
            .context("将JSON配置保存到数据库失败")?;

        info!("JSON配置导入完成");
        Ok(())
    }

    /// 导出配置为JSON
    pub async fn export_to_json(&self) -> Result<String> {
        info!("开始导出配置为JSON");

        let bundle = self
            .manager
            .load_config_bundle()
            .await
            .context("从数据库加载配置失败")?;

        let json_data = serde_json::to_string_pretty(&bundle.config).context("序列化配置为JSON失败")?;

        info!("配置导出为JSON完成");
        Ok(json_data)
    }
}

// 为Config添加额外的保存方法
impl Config {
    /// 保存配置到指定路径
    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        let config_content = self.save_with_structured_comments()?;

        // 确保父目录存在
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(path, config_content)?;
        Ok(())
    }
}
