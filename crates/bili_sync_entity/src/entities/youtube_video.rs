use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Default)]
#[sea_orm(table_name = "you_tube_video")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub source_id: i32,
    #[sea_orm(column_name = "you_tube_id")]
    pub youtube_id: String,
    pub url: String,
    pub title: String,
    pub uploader: String,
    pub thumbnail: Option<String>,
    pub published_at: Option<String>,
    pub duration_seconds: Option<i32>,
    pub episode_number: Option<i32>,
    pub is_image_post: bool,
    /// 是否为抖音「日常」（story）作品：仅在抖音 App 有接口，扫描作者源时合并。
    pub is_story: bool,
    pub is_charge_video: bool,
    pub charge_can_play: bool,
    pub download_status: String,
    pub retry_count: i32,
    pub output_path: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::youtube_source::Entity",
        from = "Column::SourceId",
        to = "super::youtube_source::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    YouTubeSource,
}

impl Related<super::youtube_source::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::YouTubeSource.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
