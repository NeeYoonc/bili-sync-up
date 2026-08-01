use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Statement;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        connection
            .execute(Statement::from_string(
                connection.get_database_backend(),
                "UPDATE you_tube_source SET download_danmaku = 1 WHERE source_type LIKE 'douyin%'".to_string(),
            ))
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 旧版本没有记录抖音源在迁移前的真实偏好，回滚时不能把用户后来
        // 主动启用的弹幕选项全部改回 false，因此数据迁移保持不可逆。
        Ok(())
    }
}
