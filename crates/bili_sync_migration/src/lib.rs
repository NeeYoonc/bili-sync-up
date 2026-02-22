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
        ]
    }
}
