//! 修复 page 表状态索引的历史 bug。
//!
//! 背景：
//! 早前的迁移 m20241228_000001_add_video_query_indexes 在创建 idx_page_status_query 时，
//! 误把列名写成了 status（page 表真实列名是 download_status）。SQLite 对双引号标识符有
//! 兼容行为，会把不存在的 "status" 当成字符串字面量，于是索引被建成了常量表达式索引：
//!     CREATE INDEX "idx_page_status_query" ON "page" ("video_id", "status")
//! 后果：
//!   1) 该索引对状态查询完全无效（PRAGMA index_info 里该列 cid = -2）；
//!   2) 在 DQS=0 / 较新版本的 SQLite 上，VACUUM / VACUUM INTO 会直接报错：
//!      no such column: "status" - should this be a string literal in single-quotes?
//!      打断后台「备份数据库（VACUUM INTO）」与自动 VACUUM；
//!      （注：app 自带的 SQLite 3.46 因 DQS 默认开启，VACUUM 能通过，但索引照样无效——
//!       所以此 bug 必现，只是 VACUUM 报错会因 SQLite 版本/编译开关而时隐时现。）
//!   3) PRAGMA integrity_check / quick_check 都不报，所以能潜伏很久。
//!
//! 本迁移负责把老库上已经建坏的索引删掉并按真实列名重建，使新库/老库最终定义一致。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 先无条件删除历史索引：无论它此前是坏定义（status 常量表达式）还是根本不存在，
        // 删完再按正确列名重建，保证幂等，最终定义一定是 (video_id, download_status)。
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_page_status_query")
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_page_status_query")
                    .table(Page::Table)
                    .col(Page::VideoId)
                    .col(Page::DownloadStatus)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 回滚同样先删再按正确列名重建（而非直接丢弃索引）。
        // 原因：本迁移修复的是一个「坏定义」——回滚不应把库退回成一个 VACUUM 会失败的坏索引，
        // 否则 down 之后又会踩到同一个 bug。保持索引存在且定义正确更安全。
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_page_status_query")
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_page_status_query")
                    .table(Page::Table)
                    .col(Page::VideoId)
                    .col(Page::DownloadStatus)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Page {
    Table,
    VideoId,
    DownloadStatus,
}
