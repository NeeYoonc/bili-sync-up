use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ImageProxyCache::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ImageProxyCache::CacheKey)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ImageProxyCache::Url).text().not_null())
                    .col(ColumnDef::new(ImageProxyCache::ContentType).string().not_null())
                    .col(ColumnDef::new(ImageProxyCache::ImageData).binary().not_null())
                    .col(ColumnDef::new(ImageProxyCache::Etag).string().not_null())
                    .col(ColumnDef::new(ImageProxyCache::CachedAtUnix).big_integer().not_null())
                    .col(ColumnDef::new(ImageProxyCache::ExpiresAtUnix).big_integer().not_null())
                    .col(ColumnDef::new(ImageProxyCache::UpdatedAtUnix).big_integer().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_image_proxy_cache_expires_at")
                    .table(ImageProxyCache::Table)
                    .col(ImageProxyCache::ExpiresAtUnix)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_image_proxy_cache_updated_at")
                    .table(ImageProxyCache::Table)
                    .col(ImageProxyCache::UpdatedAtUnix)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_image_proxy_cache_updated_at")
                    .table(ImageProxyCache::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_image_proxy_cache_expires_at")
                    .table(ImageProxyCache::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(ImageProxyCache::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ImageProxyCache {
    Table,
    CacheKey,
    Url,
    ContentType,
    ImageData,
    Etag,
    CachedAtUnix,
    ExpiresAtUnix,
    UpdatedAtUnix,
}
