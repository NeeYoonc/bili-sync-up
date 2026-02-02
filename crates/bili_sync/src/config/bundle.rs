use std::sync::Arc;

use anyhow::Result;
use handlebars::Handlebars;
use leaky_bucket::RateLimiter;

use crate::config::Config;

/// 配置包，包含所有需要热重载的组件
/// 使用 ArcSwap<ConfigBundle> 确保原子性更新
#[derive(Clone)]
pub struct ConfigBundle {
    /// 主配置结构
    pub config: Config,
    /// Handlebars 模板引擎，预编译所有模板
    pub handlebars: Handlebars<'static>,
    /// HTTP 请求限流器
    #[allow(dead_code)]
    pub rate_limiter: Arc<RateLimiter>,
}

impl ConfigBundle {
    /// 从配置构建完整的配置包
    pub fn from_config(config: Config) -> Result<Self> {
        let handlebars = Self::build_handlebars(&config)?;
        let rate_limiter = Self::build_rate_limiter(&config);

        Ok(Self {
            config,
            handlebars,
            rate_limiter: Arc::new(rate_limiter),
        })
    }

    /// 构建 Handlebars 模板引擎
    fn build_handlebars(config: &Config) -> Result<Handlebars<'static>> {
        use handlebars::handlebars_helper;
        use tracing::debug;

        fn escape_template_separators(template: &str) -> String {
            // 仅替换“模板文本”中的路径分隔符，避免破坏 Handlebars 语法（例如 {{/if}}）。
            // 规则：
            // - Handlebars 标签内（{{ ... }} / {{{ ... }}} / {{{{ ... }}}}）不做替换
            // - 标签外：
            //   - // => __UNIX_SEP__
            //   - \\\\ => __WIN_SEP__
            //   - /  => __UNIX_SEP__
            //   - \\ => __WIN_SEP__
            let mut out = String::with_capacity(template.len());
            let mut i = 0usize;
            let mut in_tag = false;
            let mut tag_end_len = 0usize;

            while i < template.len() {
                let bytes = template.as_bytes();

                if !in_tag {
                    // 进入 Handlebars 标签
                    if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                        // 支持 {{ / {{{ / {{{{ 三种
                        let mut start_len = 2usize;
                        while i + start_len < bytes.len() && bytes[i + start_len] == b'{' && start_len < 4 {
                            start_len += 1;
                        }
                        out.push_str(&template[i..i + start_len]);
                        i += start_len;
                        in_tag = true;
                        tag_end_len = start_len;
                        continue;
                    }

                    // 标签外：处理路径分隔符
                    if bytes[i] == b'/' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                            out.push_str("__UNIX_SEP__");
                            i += 2;
                        } else {
                            out.push_str("__UNIX_SEP__");
                            i += 1;
                        }
                        continue;
                    }
                    if bytes[i] == b'\\' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            out.push_str("__WIN_SEP__");
                            i += 2;
                        } else {
                            out.push_str("__WIN_SEP__");
                            i += 1;
                        }
                        continue;
                    }

                    // 其它字符：按 UTF-8 字符复制
                    let ch = template[i..].chars().next().unwrap();
                    out.push(ch);
                    i += ch.len_utf8();
                    continue;
                }

                // Handlebars 标签内：寻找结束符
                if bytes[i] == b'}' && tag_end_len > 0 {
                    let mut ok = true;
                    for k in 0..tag_end_len {
                        if i + k >= bytes.len() || bytes[i + k] != b'}' {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        out.push_str(&template[i..i + tag_end_len]);
                        i += tag_end_len;
                        in_tag = false;
                        tag_end_len = 0;
                        continue;
                    }
                }

                // 标签内其它字符
                let ch = template[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }

            out
        }

        debug!("开始构建Handlebars模板引擎...");
        let mut handlebars = Handlebars::new();

        // 禁用HTML转义，避免文件名中的特殊字符被转义为HTML实体
        // 例如：避免 "=" 被转义为 "&#x3D;"
        handlebars.register_escape_fn(|s| s.to_string());
        debug!("已禁用Handlebars HTML转义");

        // 注册自定义 helper
        handlebars_helper!(truncate: |s: String, len: usize| {
            if s.chars().count() > len {
                s.chars().take(len).collect::<String>()
            } else {
                s.to_string()
            }
        });
        handlebars.register_helper("truncate", Box::new(truncate));
        debug!("Handlebars helper 'truncate' 已注册");

        // 注册所有必需的模板
        // 使用 to_string() 转换 Cow<'static, str> 为 &'static str
        let video_name = Box::leak(config.video_name.to_string().into_boxed_str());
        let page_name = Box::leak(config.page_name.to_string().into_boxed_str());
        let multi_page_name = Box::leak(config.multi_page_name.to_string().into_boxed_str());
        let bangumi_name = Box::leak(config.bangumi_name.to_string().into_boxed_str());
        let collection_unified_name = Box::leak(config.collection_unified_name.to_string().into_boxed_str());
        let folder_structure = Box::leak(config.folder_structure.to_string().into_boxed_str());
        let bangumi_folder_name = Box::leak(config.bangumi_folder_name.to_string().into_boxed_str());

        // 区分Unix风格和Windows风格的路径分隔符（仅替换标签外文本，避免破坏 {{/if}} 等语法）
        let safe_video_name = escape_template_separators(video_name);
        let safe_page_name = escape_template_separators(page_name);
        let safe_multi_page_name = escape_template_separators(multi_page_name);
        let safe_bangumi_name = escape_template_separators(bangumi_name);
        let safe_collection_unified_name = escape_template_separators(collection_unified_name);
        let safe_folder_structure = escape_template_separators(folder_structure);
        let safe_bangumi_folder_name = escape_template_separators(bangumi_folder_name);

        // 注册模板并记录日志
        handlebars.register_template_string("video", &safe_video_name)?;
        debug!("模板 'video' 已注册: '{}' -> '{}'", video_name, safe_video_name);

        handlebars.register_template_string("page", &safe_page_name)?;
        debug!("模板 'page' 已注册: '{}' -> '{}'", page_name, safe_page_name);

        handlebars.register_template_string("multi_page", &safe_multi_page_name)?;
        debug!(
            "模板 'multi_page' 已注册: '{}' -> '{}'",
            multi_page_name, safe_multi_page_name
        );

        handlebars.register_template_string("bangumi", &safe_bangumi_name)?;
        debug!("模板 'bangumi' 已注册: '{}' -> '{}'", bangumi_name, safe_bangumi_name);

        handlebars.register_template_string("collection_unified", &safe_collection_unified_name)?;
        debug!(
            "模板 'collection_unified' 已注册: '{}' -> '{}'",
            collection_unified_name, safe_collection_unified_name
        );

        handlebars.register_template_string("folder_structure", &safe_folder_structure)?;
        debug!(
            "模板 'folder_structure' 已注册: '{}' -> '{}'",
            folder_structure, safe_folder_structure
        );

        handlebars.register_template_string("bangumi_folder", &safe_bangumi_folder_name)?;
        debug!(
            "模板 'bangumi_folder' 已注册: '{}' -> '{}'",
            bangumi_folder_name, safe_bangumi_folder_name
        );

        debug!("Handlebars模板引擎构建完成，共注册 {} 个模板", 7);
        Ok(handlebars)
    }

    /// 构建速率限制器
    fn build_rate_limiter(config: &Config) -> RateLimiter {
        if let Some(rate_limit) = &config.concurrent_limit.rate_limit {
            RateLimiter::builder()
                .max(rate_limit.limit)
                .refill(rate_limit.limit)
                .interval(std::time::Duration::from_millis(rate_limit.duration))
                .build()
        } else {
            // 默认限流器：每250ms允许4个请求
            RateLimiter::builder()
                .max(4)
                .refill(4)
                .interval(std::time::Duration::from_millis(250))
                .build()
        }
    }

    /// 检查配置是否有效
    #[cfg(not(test))]
    pub fn validate(&self) -> bool {
        // 复用现有的配置检查逻辑
        self.config.check()
    }

    /// 测试环境下的验证方法
    #[cfg(test)]
    pub fn validate(&self) -> bool {
        // 在测试环境下总是返回true
        true
    }

    /// 获取配置值的便捷方法
    #[allow(dead_code)]
    pub fn get_video_name_template(&self) -> &str {
        &self.config.video_name
    }

    #[allow(dead_code)]
    pub fn get_page_name_template(&self) -> &str {
        &self.config.page_name
    }

    #[allow(dead_code)]
    pub fn get_bind_address(&self) -> &str {
        &self.config.bind_address
    }

    #[allow(dead_code)]
    pub fn get_interval(&self) -> u64 {
        self.config.interval
    }

    /// 渲染模板的便捷方法（使用path_safe_render确保分隔符正确处理）
    #[allow(dead_code)]
    pub fn render_template(&self, template_name: &str, data: &serde_json::Value) -> Result<String> {
        use crate::utils::filenamify::filenamify_with_options;

        // 两阶段处理：
        // 1. 先渲染模板，保护模板路径分隔符
        let rendered = self.handlebars.render(template_name, data)?;

        // 2. 对整个渲染结果进行安全化，保护模板分隔符
        let safe_rendered = filenamify_with_options(&rendered, true);

        // 3. 最后处理路径分隔符
        #[cfg(windows)]
        {
            Ok(safe_rendered.replace("__UNIX_SEP__", "/").replace("__WIN_SEP__", "\\"))
        }
        #[cfg(not(windows))]
        {
            Ok(safe_rendered.replace("__UNIX_SEP__", "/").replace("__WIN_SEP__", "_"))
        }
    }

    /// 安全渲染模板的通用方法（修复原始斜杠分割问题）
    fn render_template_safe(&self, template_name: &str, data: &serde_json::Value) -> Result<String> {
        use crate::utils::filenamify::filenamify_with_options;

        // 两阶段处理（修复原始斜杠分割问题）：
        // 1. 先渲染模板，模板分隔符已转换为 __UNIX_SEP__ 等占位符
        let rendered = self.handlebars.render(template_name, data)?;

        // 2. 对整个渲染结果进行安全化，保护模板分隔符
        // filenamify_with_options 已经正确处理了内容中的斜杠
        let safe_rendered = filenamify_with_options(&rendered, true);

        // 3. 最后处理模板路径分隔符，将占位符转换为真实的路径分隔符
        #[cfg(windows)]
        {
            Ok(safe_rendered
                .replace("__UNIX_SEP__", "/")  // 模板路径分隔符 → 真实分隔符
                .replace("__WIN_SEP__", "\\"))
        }
        #[cfg(not(windows))]
        {
            Ok(safe_rendered
                .replace("__UNIX_SEP__", "/")  // 模板路径分隔符 → 真实分隔符
                .replace("__WIN_SEP__", "_"))
        }
    }

    /// 渲染视频名称模板的便捷方法
    pub fn render_video_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("video", data)
    }

    /// 渲染分页名称模板的便捷方法
    pub fn render_page_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("page", data)
    }

    /// 渲染多P视频分页名称模板的便捷方法
    pub fn render_multi_page_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("multi_page", data)
    }

    /// 渲染合集统一模式分页名称模板的便捷方法
    pub fn render_collection_unified_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("collection_unified", data)
    }

    /// 渲染番剧名称模板的便捷方法
    #[allow(dead_code)]
    pub fn render_bangumi_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("bangumi", data)
    }

    /// 渲染番剧文件夹名称模板的便捷方法
    pub fn render_bangumi_folder_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("bangumi_folder", data)
    }

    /// 渲染文件夹结构模板的便捷方法
    pub fn render_folder_structure_template(&self, data: &serde_json::Value) -> Result<String> {
        self.render_template_safe("folder_structure", data)
    }
}

impl std::fmt::Debug for ConfigBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigBundle")
            .field("config", &"<Config instance>")
            .field("handlebars", &"<Handlebars instance>")
            .field("rate_limiter", &"<RateLimiter instance>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;
    use std::borrow::Cow;

    #[test]
    fn test_video_template_path_separator_handling() {
        // 设置包含路径分隔符的模板，模拟用户问题中的场景
        let config = Config {
            video_name: Cow::Borrowed("{{upper_name}}/{{title}}"),
            ..Default::default()
        };
        let bundle = ConfigBundle::from_config(config).unwrap();

        // 测试视频文件名模板中的路径分隔符处理
        let test_data = json!({
            "upper_name": "ZHY2020",
            "title": "【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身/ドッペルゲンガー」孤独摇滚！总集剧场版Re:Re: OP Lyric MV [HiRes 48kHz/24bit]"
        });

        let result = bundle.render_video_template(&test_data).unwrap();

        // 应该包含路径分隔符，而不是下划线
        #[cfg(windows)]
        {
            // Windows下应该包含正斜杠分隔符
            assert!(
                result.contains("/"),
                "Windows系统下路径分隔符应该是 '/'，实际结果: {}",
                result
            );
            assert!(
                !result.contains("ZHY2020__"),
                "不应该出现双下划线连接，实际结果: {}",
                result
            );
        }
        #[cfg(not(windows))]
        {
            // 非Windows系统下应该包含正斜杠分隔符
            assert!(
                result.contains("/"),
                "非Windows系统下路径分隔符应该是 '/'，实际结果: {}",
                result
            );
            assert!(
                !result.contains("ZHY2020__"),
                "不应该出现双下划线连接，实际结果: {}",
                result
            );
        }

        // 验证特殊字符被正确处理（内容中的分隔符应该被转换为安全字符）
        assert!(
            result.contains("[分身_ドッペルゲンガー]"),
            "特殊字符应该被正确处理，实际结果: {}",
            result
        );
    }

    #[test]
    fn test_template_reload_with_different_configs() {
        let test_data = json!({
            "upper_name": "TestUpper",
            "title": "TestVideo"
        });

        // 创建第一个配置
        let config1 = Config {
            video_name: Cow::Borrowed("{{upper_name}}-{{title}}"),
            ..Default::default()
        };
        let bundle1 = ConfigBundle::from_config(config1).unwrap();

        let result1 = bundle1.render_video_template(&test_data).unwrap();
        assert_eq!(result1, "TestUpper-TestVideo");

        // 创建第二个配置，模拟配置更改
        let config2 = Config {
            video_name: Cow::Borrowed("{{upper_name}}/{{title}}"),
            ..Default::default()
        };
        let bundle2 = ConfigBundle::from_config(config2).unwrap();

        let result2 = bundle2.render_video_template(&test_data).unwrap();
        assert!(result2.contains("/"), "更新后的模板应该包含路径分隔符: {}", result2);
        assert_eq!(result2, "TestUpper/TestVideo");

        // 验证两个bundle的结果不同
        assert_ne!(result1, result2, "不同配置应该产生不同的渲染结果");
    }

    #[test]
    fn test_template_render_consistency() {
        let config = Config {
            video_name: Cow::Borrowed("{{upper_name}}/{{title}}"),
            page_name: Cow::Borrowed("{{upper_name}}/{{title}}/Page{{page}}"),
            ..Default::default()
        };

        let bundle = ConfigBundle::from_config(config).unwrap();

        let test_data = json!({
            "upper_name": "UP主名称",
            "title": "视频标题",
            "page": "01"
        });

        // 渲染不同的模板
        let video_result = bundle.render_video_template(&test_data).unwrap();
        let page_result = bundle.render_page_template(&test_data).unwrap();

        // 验证路径分隔符一致性
        assert!(video_result.contains("/"), "video模板应该包含路径分隔符");
        assert!(page_result.contains("/"), "page模板应该包含路径分隔符");

        // 验证基础路径一致
        assert!(page_result.starts_with(&video_result), "page路径应该以video路径为前缀");
    }

    #[test]
    fn test_content_slash_handling() {
        // 创建一个测试配置
        let config = Config {
            video_name: Cow::Borrowed("{{upper_name}}/{{title}}"),
            ..Default::default()
        };

        let bundle = ConfigBundle::from_config(config).unwrap();

        // 测试包含斜杠的数据
        let data = json!({
            "upper_name": "ZHY2020",
            "title": "【𝟒𝐊 𝐇𝐢𝐑𝐞𝐬】「分身/ドッペルゲンガー」孤独摇滚！总集剧场版Re:Re:"
        });

        let result = bundle.render_video_template(&data).unwrap();

        // 验证结果：应该创建正确的目录结构，内容中的斜杠应该被转换为下划线
        // 期望：ZHY2020/[正确处理的标题]，其中标题中的 / 被转换为 _
        assert!(
            result.starts_with("ZHY2020/"),
            "应该以 ZHY2020/ 开头，实际结果: {}",
            result
        );
        assert!(
            !result.contains("分身/ドッペルゲンガー"),
            "原始斜杠应该被处理，实际结果: {}",
            result
        );
        assert!(
            result.contains("分身_ドッペルゲンガー"),
            "斜杠应该变成下划线，实际结果: {}",
            result
        );

        // 确保只有一个路径分隔符
        let slash_count = result.matches('/').count();
        assert_eq!(
            slash_count, 1,
            "应该只有一个路径分隔符，但发现了 {}，结果: {}",
            slash_count, result
        );
    }

    #[test]
    fn test_html_escape_disabled() {
        // 测试Handlebars HTML转义已被正确禁用
        let config = Config {
            video_name: Cow::Borrowed("{{upper_name}}"),
            ..Default::default()
        };

        let bundle = ConfigBundle::from_config(config).unwrap();

        // 测试包含等号的数据（等号不应该被HTML转义）
        let data = json!({
            "upper_name": "=咬人猫="
        });

        let result = bundle.render_video_template(&data).unwrap();

        // 打印结果用于调试
        println!("修复后的渲染结果: {}", result);

        // 验证HTML转义已被禁用
        assert!(
            !result.contains("&#x3D;"),
            "HTML转义应该被禁用，等号不应该被转义为 &#x3D;，实际结果: {}",
            result
        );

        // 验证原始等号保持不变
        assert_eq!(result, "=咬人猫=", "等号应该保持原样，实际结果: {}", result);
    }
}
