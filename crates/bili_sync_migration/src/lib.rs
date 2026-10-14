pub use sea_orm_migration::prelude::*;

mod m20240322_000001_create_table;
mod m20240505_130850_add_collection;
mod m20240709_130914_watch_later;
mod m20240724_161008_submission;
mod m20241228_000001_add_video_query_indexes;
mod m20250104_000001_add_selected_videos_field;
mod m20250104_000002_add_auto_download_field;
mod m20250122_062926_add_latest_row_at;
mod m20250519_000001_add_source_id;
mod m20250520_000001_add_download_all_seasons;
mod m20250525_000001_add_bangumi_templates;
mod m20250525_000002_add_season_number;
mod m20250525_000003_add_selected_seasons;
mod m20250531_000001_fix_fid_type;
mod m20250601_000001_fix_compatibility;
mod m20250612_090826_add_enabled;
mod m20250613_000001_add_performance_indexes;
mod m20250613_000002_add_enabled_field;
mod m20250613_043257_add_config;
mod m20250616_000001_create_config_tables;
mod m20250624_000001_add_deleted_field;
mod m20250624_000002_add_scan_deleted_videos_field;
mod m20250628_000001_create_task_queue;
mod m20250701_000001_add_share_copy_field;
mod m20250701_000002_add_show_season_type_field;
mod m20250705_000001_add_actors_field;
mod m20250708_000001_add_collection_season_structure;
mod m20250710_000001_add_bangumi_season_structure;
mod m20250712_080013_add_video_created_at_index;
mod m20250717_000001_add_staff_info;
mod m20250722_000001_add_bangumi_cache_fields;
mod m20250726_000001_unify_time_format;
mod m20250807_000001_add_video_cid;
mod m20250903_094454_add_rule_and_should_download;
mod m20250914_000001_fix_video_unique_index_for_bangumi;
mod m20250921_000001_add_collection_cover;
mod m20251009_123713_add_use_dynamic_api;
mod m20251210_000001_add_keyword_filters_field;
mod m20251211_000001_add_keyword_filter_mode_field;
mod m20251211_000002_split_keyword_filters;
mod m20251211_000003_add_keyword_case_sensitive;
mod m20260101_000001_add_audio_only_field;
mod m20260101_000002_add_download_toggles;
mod m20260103_000001_add_ai_rename_toggle;
mod m20260103_000002_add_ai_rename_prompts;
mod m20260104_000001_create_ai_conversation_history;
mod m20260104_000002_add_audio_only_m4a_only_and_flat_folder;
mod m20260108_000001_add_ai_rename_advanced_options;
mod m20260125_000001_migrate_legacy_config;
mod m20260125_000002_add_use_dynamic_api;
mod m20260125_000003_add_dynamic_api_full_synced;
mod m20260127_000001_add_submission_scan_state;
mod m20260202_000001_add_ai_rename_parent_dir;
mod m20260203_000001_create_collection_season_mapping;
mod m20260220_000001_add_page_play_stream_cache;
mod m20260222_000001_create_image_proxy_cache;
mod m20260222_000002_add_video_submission_membership_state;
mod m20260307_000001_add_advanced_keyword_filters;
mod m20260307_000002_add_collection_aggregate_fields;
mod m20260308_000001_add_collection_episode_order_strategy;
mod m20260308_000002_add_video_charge_flags;
mod m20260314_000001_clear_image_proxy_image_data;
mod m20260328_000001_add_scan_deleted_videos_once_field;
mod m20260330_000001_add_video_file_size_fields;
mod m20260414_000001_add_danmaku_sync_fields;
mod m20260415_000001_add_split_chapters_to_sources;
mod m20260704_000001_add_ai_subtitle_settings;
mod m20260718_000001_add_source_filter_option;
mod m20260719_000001_add_source_download_charge_videos;
mod m20260729_000001_create_youtube_sources;
mod m20260729_000002_align_youtube_download_lifecycle;
mod m20260729_000003_integrate_youtube_source_options;
mod m20260730_000001_add_youtube_selected_videos;
mod m20260730_000002_add_youtube_source_selection_state;
mod m20260802_000001_add_youtube_video_image_post;
mod m20260802_000002_add_youtube_source_ai_rename;
mod m20260802_000003_enable_douyin_danmaku;
mod m20260802_000004_add_youtube_scan_deleted;
mod m20260804_000001_add_external_episode_number;
mod m20260807_000001_add_youtube_video_charge_flags;
mod m20260821_000001_add_video_skip_reason;
mod m20260826_000001_add_youtube_video_story;
mod m20260903_000001_add_youtube_video_task_status;
mod m20260904_000001_backfill_external_task_status_by_files;
mod m20260905_000001_backfill_external_moved_media_as_completed;
mod m20260913_000001_retry_failed_tiktok_image_posts;
mod m20261014_000001_fix_page_status_index;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240322_000001_create_table::Migration),
            Box::new(m20240505_130850_add_collection::Migration),
            Box::new(m20240709_130914_watch_later::Migration),
            Box::new(m20240724_161008_submission::Migration),
            Box::new(m20250122_062926_add_latest_row_at::Migration),
            Box::new(m20250519_000001_add_source_id::Migration),
            Box::new(m20250520_000001_add_download_all_seasons::Migration),
            Box::new(m20250525_000001_add_bangumi_templates::Migration),
            Box::new(m20250525_000002_add_season_number::Migration),
            Box::new(m20250525_000003_add_selected_seasons::Migration),
            Box::new(m20250531_000001_fix_fid_type::Migration),
            Box::new(m20250601_000001_fix_compatibility::Migration),
            Box::new(m20250612_090826_add_enabled::Migration),
            Box::new(m20250613_043257_add_config::Migration),
            Box::new(m20250613_000001_add_performance_indexes::Migration),
            Box::new(m20250613_000002_add_enabled_field::Migration),
            Box::new(m20250616_000001_create_config_tables::Migration),
            Box::new(m20250624_000001_add_deleted_field::Migration),
            Box::new(m20250624_000002_add_scan_deleted_videos_field::Migration),
            Box::new(m20250628_000001_create_task_queue::Migration),
            Box::new(m20241228_000001_add_video_query_indexes::Migration),
            Box::new(m20250701_000001_add_share_copy_field::Migration),
            Box::new(m20250701_000002_add_show_season_type_field::Migration),
            Box::new(m20250705_000001_add_actors_field::Migration),
            Box::new(m20250708_000001_add_collection_season_structure::Migration),
            Box::new(m20250710_000001_add_bangumi_season_structure::Migration),
            Box::new(m20250712_080013_add_video_created_at_index::Migration),
            Box::new(m20250104_000001_add_selected_videos_field::Migration),
            Box::new(m20250104_000002_add_auto_download_field::Migration),
            Box::new(m20250903_094454_add_rule_and_should_download::Migration),
            Box::new(m20250717_000001_add_staff_info::Migration),
            Box::new(m20250722_000001_add_bangumi_cache_fields::Migration),
            Box::new(m20250726_000001_unify_time_format::Migration),
            Box::new(m20250807_000001_add_video_cid::Migration),
            Box::new(m20250914_000001_fix_video_unique_index_for_bangumi::Migration),
            Box::new(m20250921_000001_add_collection_cover::Migration),
            Box::new(m20251009_123713_add_use_dynamic_api::Migration),
            Box::new(m20251210_000001_add_keyword_filters_field::Migration),
            Box::new(m20251211_000001_add_keyword_filter_mode_field::Migration),
            Box::new(m20251211_000002_split_keyword_filters::Migration),
            Box::new(m20251211_000003_add_keyword_case_sensitive::Migration),
            Box::new(m20260101_000001_add_audio_only_field::Migration),
            Box::new(m20260101_000002_add_download_toggles::Migration),
            Box::new(m20260103_000001_add_ai_rename_toggle::Migration),
            Box::new(m20260103_000002_add_ai_rename_prompts::Migration),
            Box::new(m20260104_000001_create_ai_conversation_history::Migration),
            Box::new(m20260104_000002_add_audio_only_m4a_only_and_flat_folder::Migration),
            Box::new(m20260108_000001_add_ai_rename_advanced_options::Migration),
            Box::new(m20260125_000001_migrate_legacy_config::Migration),
            Box::new(m20260125_000002_add_use_dynamic_api::Migration),
            Box::new(m20260125_000003_add_dynamic_api_full_synced::Migration),
            Box::new(m20260127_000001_add_submission_scan_state::Migration),
            Box::new(m20260202_000001_add_ai_rename_parent_dir::Migration),
            Box::new(m20260203_000001_create_collection_season_mapping::Migration),
            Box::new(m20260220_000001_add_page_play_stream_cache::Migration),
            Box::new(m20260222_000002_add_video_submission_membership_state::Migration),
            Box::new(m20260222_000001_create_image_proxy_cache::Migration),
            Box::new(m20260307_000001_add_advanced_keyword_filters::Migration),
            Box::new(m20260307_000002_add_collection_aggregate_fields::Migration),
            Box::new(m20260308_000001_add_collection_episode_order_strategy::Migration),
            Box::new(m20260308_000002_add_video_charge_flags::Migration),
            Box::new(m20260314_000001_clear_image_proxy_image_data::Migration),
            Box::new(m20260328_000001_add_scan_deleted_videos_once_field::Migration),
            Box::new(m20260330_000001_add_video_file_size_fields::Migration),
            Box::new(m20260414_000001_add_danmaku_sync_fields::Migration),
            Box::new(m20260415_000001_add_split_chapters_to_sources::Migration),
            Box::new(m20260704_000001_add_ai_subtitle_settings::Migration),
            Box::new(m20260718_000001_add_source_filter_option::Migration),
            Box::new(m20260719_000001_add_source_download_charge_videos::Migration),
            Box::new(m20260729_000001_create_youtube_sources::Migration),
            Box::new(m20260729_000002_align_youtube_download_lifecycle::Migration),
            Box::new(m20260729_000003_integrate_youtube_source_options::Migration),
            Box::new(m20260730_000001_add_youtube_selected_videos::Migration),
            Box::new(m20260730_000002_add_youtube_source_selection_state::Migration),
            Box::new(m20260802_000001_add_youtube_video_image_post::Migration),
            Box::new(m20260802_000002_add_youtube_source_ai_rename::Migration),
            Box::new(m20260802_000003_enable_douyin_danmaku::Migration),
            Box::new(m20260802_000004_add_youtube_scan_deleted::Migration),
            Box::new(m20260804_000001_add_external_episode_number::Migration),
            Box::new(m20260807_000001_add_youtube_video_charge_flags::Migration),
            Box::new(m20260821_000001_add_video_skip_reason::Migration),
            Box::new(m20260826_000001_add_youtube_video_story::Migration),
            Box::new(m20260903_000001_add_youtube_video_task_status::Migration),
            Box::new(m20260904_000001_backfill_external_task_status_by_files::Migration),
            Box::new(m20260905_000001_backfill_external_moved_media_as_completed::Migration),
            Box::new(m20260913_000001_retry_failed_tiktok_image_posts::Migration),
            Box::new(m20261014_000001_fix_page_status_index::Migration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{
        ConnectionTrait, Database, DatabaseConnection, DbBackend, QueryResult, Statement,
    };

    async fn query_all_sql(db: &DatabaseConnection, sql: String) -> Vec<QueryResult> {
        let backend: DbBackend = db.get_database_backend();
        ConnectionTrait::query_all(db, Statement::from_string(backend, sql))
            .await
            .expect("查询失败")
    }

    // 执行一条 DDL/DML，返回是否成功（用于探测性语句，不 panic）。
    async fn exec(db: &DatabaseConnection, sql: &str) -> bool {
        ConnectionTrait::execute_unprepared(db, sql).await.is_ok()
    }

    // 取某个对象（索引）在 sqlite_master 里的 SQL 定义文本。
    async fn index_sql(db: &DatabaseConnection, name: &str) -> Option<String> {
        let backend: DbBackend = db.get_database_backend();
        let rows = ConnectionTrait::query_all(
            db,
            Statement::from_sql_and_values(
                backend,
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?",
                [name.into()],
            ),
        )
        .await
        .expect("查询 sqlite_master 失败");
        rows.into_iter()
            .next()
            .and_then(|row| row.try_get::<String>("", "sql").ok())
    }

    // 对指定索引执行 PRAGMA index_info，返回索引列里 cid = -2 的个数。
    // cid = -2 表示该索引列是一个「表达式」而非真实列——坏索引把不存在的列名当成字符串
    // 字面量后就是这个形态（3.46 与 3.50 两个版本都会返回 cid = -2，跨版本稳定）。
    async fn expression_col_count(db: &DatabaseConnection, index_name: &str) -> usize {
        let sql = format!("PRAGMA index_info(\"{}\")", index_name.replace('"', "\"\""));
        query_all_sql(db, sql)
            .await
            .into_iter()
            .filter(|row| {
                row.try_get::<i64>("", "cid")
                    .map(|cid| cid == -2)
                    .unwrap_or(false)
            })
            .count()
    }

    async fn integrity_check_is_ok(db: &DatabaseConnection) -> bool {
        let rows = query_all_sql(db, "PRAGMA integrity_check".to_owned()).await;
        // integrity_check 正常时只返回一列、一行、值为 "ok"。
        rows.len() == 1
            && rows[0]
                .try_get_by_index::<String>(0)
                .map(|v| v == "ok")
                .unwrap_or(false)
    }

    // 新库：迁移跑完后 idx_page_status_query 定义必须正确，且不含表达式列，VACUUM 正常。
    //
    // 注意：这里只针对 page 表的这条索引做「无 cid=-2」断言，不做全库扫描。
    // 原因：库里存在一个合法的表达式索引 idx_video_unique（ifnull(...) 唯一索引，
    // 故意用 ifnull 处理 NULL），它本来就有 cid=-2 的列，全库扫描会误伤。
    #[tokio::test]
    async fn test_fresh_migrations_index_is_correct() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("连接内存数据库失败");
        Migrator::up(&db, None).await.expect("执行迁移失败");

        // idx_page_status_query 的 SQL 必须含 download_status，且不含被误用的 "status"。
        let sql = index_sql(&db, "idx_page_status_query")
            .await
            .expect("idx_page_status_query 不存在");
        assert!(
            sql.contains("download_status"),
            "索引定义应引用 download_status，实际为: {sql}"
        );
        assert!(
            !sql.contains("\"status\""),
            "索引定义不应出现被误用的 \"status\"，实际为: {sql}"
        );

        // 该索引不允许存在表达式列（cid = -2）。
        let expr_cols = expression_col_count(&db, "idx_page_status_query").await;
        assert_eq!(
            expr_cols, 0,
            "idx_page_status_query 含有 {expr_cols} 个表达式列（引用了不存在的列）"
        );

        // VACUUM 必须成功（在该 SQLite 版本下；版本差异见 test_legacy_... 的说明）。
        db.execute_unprepared("VACUUM")
            .await
            .expect("VACUUM 失败，可能存在坏索引");

        assert!(
            integrity_check_is_ok(&db).await,
            "PRAGMA integrity_check 未返回 ok"
        );
    }

    // 老库：手工造出历史坏索引，确认它是「表达式索引」形态，再用修复迁移修好。
    //
    // 关于 VACUUM 的版本差异（重要，勿改成「VACUUM 必须 Err」）：
    //   - app 链接的 libsqlite3-sys 0.30.1 内置 SQLite 3.46.0，DQS 默认开启，
    //     对这种坏索引 VACUUM / VACUUM INTO 都能通过，但索引本身照样无效；
    //   - DQS=0（如 SQLite 3.50.2 的 CLI 构建）或更新版本上，同一份库 VACUUM 会直接报
    //     no such column: "status" - should this be a string literal in single-quotes?
    // 所以「VACUUM 必 Err」不是跨版本成立的判定，本测试用「存在 cid=-2 表达式列」来判定坏索引，
    // 该判定在 3.46 与 3.50 上都成立。VACUUM 失败的证据见修复报告（真实数据 + 3.50.2 CLI）。
    #[tokio::test]
    async fn test_legacy_broken_index_is_repaired() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("连接内存数据库失败");
        Migrator::up(&db, None).await.expect("执行迁移失败");

        // 注入历史坏定义（列名写成 status）。
        db.execute_unprepared("DROP INDEX IF EXISTS idx_page_status_query")
            .await
            .expect("删除正确索引失败");

        // 优先用普通 CREATE INDEX 注入；若当前引擎是 DQS=0 构建（会把 "status" 判为不存在列而拒绝），
        // 退回到用 PRAGMA writable_schema 直接改写 sqlite_master，把坏定义原样写进去。
        let bad_sql = "CREATE INDEX \"idx_page_status_query\" ON \"page\" (\"video_id\", \"status\")";
        let created_directly = exec(&db, bad_sql).await;
        if !created_directly {
            // DQS=0 构建下走这条路：直接改写 sqlite_master（需要 writable_schema）。
            let wrote = exec(&db, "PRAGMA writable_schema=ON").await
                && exec(
                    &db,
                    &format!(
                        "INSERT INTO sqlite_master(type,name,tbl_name,rootpage,sql) \
                         VALUES('index','idx_page_status_query','page',0,'{bad_sql}')"
                    ),
                )
                .await
                && exec(&db, "PRAGMA writable_schema=OFF").await;
            if !wrote {
                panic!(
                    "无法注入历史坏索引：直接 CREATE 被拒（DQS=0 构建），\
                     且 writable_schema 改写 sqlite_master 也失败。\
                     本测试不允许静默通过，请修正注入手段。"
                );
            }
            // 避免污染后续连接：本次注入只作用于当前内存库连接。
        }

        // 跨版本稳定判定：坏索引的第二列是表达式列（cid = -2）。
        let expr_cols = expression_col_count(&db, "idx_page_status_query").await;
        assert!(
            expr_cols > 0,
            "注入坏索引后本应存在表达式列（cid = -2），实际为 0，说明坏定义未生效"
        );

        // 调用修复迁移把索引改回正确定义。
        let manager = SchemaManager::new(&db);
        m20261014_000001_fix_page_status_index::Migration
            .up(&manager)
            .await
            .expect("修复迁移执行失败");

        // 修复后：SQL 含 download_status、无表达式列、VACUUM 成功。
        let sql = index_sql(&db, "idx_page_status_query")
            .await
            .expect("修复后索引不存在");
        assert!(
            sql.contains("download_status"),
            "修复后索引应引用 download_status，实际为: {sql}"
        );
        assert_eq!(
            expression_col_count(&db, "idx_page_status_query").await,
            0,
            "修复后 idx_page_status_query 仍存在表达式列"
        );
        db.execute_unprepared("VACUUM")
            .await
            .expect("修复后 VACUUM 失败");
        assert!(
            integrity_check_is_ok(&db).await,
            "修复后 PRAGMA integrity_check 未返回 ok"
        );
    }
}

