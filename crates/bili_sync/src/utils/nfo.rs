use crate::utils::time_format::parse_time_string;
use anyhow::Result;
use bili_sync_entity::*;
use chrono::NaiveDateTime;
use quick_xml::events::{BytesCData, BytesText};
use quick_xml::writer::Writer;
use quick_xml::Error;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::config::{EmptyUpperStrategy, NFOConfig, NFOTimeType};

#[allow(clippy::upper_case_acronyms)]
pub enum NFO<'a> {
    Movie(Movie<'a>),
    TVShow(TVShow<'a>),
    Upper(Upper),
    Episode(Episode<'a>),
    Season(Season<'a>),
}

pub struct Movie<'a> {
    pub name: &'a str,
    pub original_title: &'a str,
    pub intro: &'a str,
    pub bvid: &'a str,
    pub upper_id: i64,
    pub upper_name: &'a str,
    pub aired: NaiveDateTime,
    pub premiered: NaiveDateTime,
    pub tags: Option<Vec<String>>,
    pub user_rating: Option<f32>,
    pub mpaa: Option<&'a str>,
    pub country: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub director: Option<&'a str>,
    pub credits: Option<&'a str>,
    pub duration: Option<i32>,                     // 视频时长（分钟）
    pub view_count: Option<i64>,                   // 播放量
    pub like_count: Option<i64>,                   // 点赞数
    pub category: i32,                             // 视频分类（用于番剧检测）
    pub tagline: Option<String>,                   // 标语/副标题（从share_copy提取）
    pub set: Option<String>,                       // 系列名称
    pub sorttitle: Option<String>,                 // 排序标题
    pub actors_info: Option<String>,               // 演员信息字符串（从API获取，番剧用）
    pub staff_info: Option<&'a serde_json::Value>, // 联合投稿成员信息（JSON格式）
    pub cover_url: &'a str,                        // 封面图片URL
    pub fanart_url: Option<&'a str>,               // 背景图片URL
    pub upper_face_url: Option<&'a str>,           // UP主头像URL（用于演员thumb）
}

pub struct TVShow<'a> {
    pub name: &'a str,
    pub original_title: &'a str,
    pub intro: &'a str,
    pub bvid: &'a str,
    pub upper_id: i64,
    pub upper_name: &'a str,
    pub aired: NaiveDateTime,
    pub premiered: NaiveDateTime,
    pub tags: Option<Vec<String>>,
    pub user_rating: Option<f32>,
    pub mpaa: Option<&'a str>,
    pub country: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub status: Option<&'a str>, // 播出状态：Continuing, Ended
    pub total_seasons: Option<i32>,
    pub total_episodes: Option<i32>,
    pub duration: Option<i32>, // 视频时长（分钟）
    pub view_count: Option<i64>,
    pub like_count: Option<i64>,
    pub category: i32,                             // 视频分类（用于番剧检测）
    pub tagline: Option<String>,                   // 标语/副标题（从share_copy提取）
    pub set: Option<String>,                       // 系列名称
    pub sorttitle: Option<String>,                 // 排序标题
    pub actors_info: Option<String>,               // 演员信息字符串（从API获取，番剧用）
    pub staff_info: Option<&'a serde_json::Value>, // 联合投稿成员信息（JSON格式）
    pub cover_url: &'a str,                        // 封面图片URL
    pub fanart_url: Option<&'a str>,               // 背景图片URL
    pub upper_face_url: Option<&'a str>,           // UP主头像URL（用于演员thumb）
    pub season_id: Option<String>,                 // 番剧季度ID（从API获取）
    pub media_id: Option<i64>,                     // 媒体ID（从API获取）
}

pub struct Upper {
    pub upper_id: String,
    pub upper_name: String,
    pub pubtime: NaiveDateTime,
}

pub struct Episode<'a> {
    pub name: &'a str,
    pub original_title: &'a str,
    pub pid: String,
    pub plot: Option<&'a str>,
    pub season: i32,
    pub episode_number: i32,
    pub aired: Option<NaiveDateTime>,
    pub duration: Option<i32>, // 时长（分钟）
    pub user_rating: Option<f32>,
    pub director: Option<&'a str>,
    pub credits: Option<&'a str>,
    pub bvid: &'a str,               // B站视频ID
    pub category: i32,               // 视频分类（用于番剧检测）
    pub mpaa: Option<&'a str>,       // 年龄分级
    pub country: Option<&'a str>,    // 国家
    pub studio: Option<&'a str>,     // 制作工作室
    pub genres: Option<Vec<String>>, // 类型标签
    pub thumb_url: Option<&'a str>,  // 缩略图URL
    pub fanart_url: Option<&'a str>, // 背景图URL
}

pub struct Season<'a> {
    pub name: &'a str,
    pub original_title: &'a str,
    pub intro: &'a str,
    pub season_number: i32,
    pub bvid: &'a str,
    pub upper_id: i64,
    pub upper_name: &'a str,
    pub aired: NaiveDateTime,
    pub premiered: NaiveDateTime,
    pub tags: Option<Vec<String>>,
    pub user_rating: Option<f32>,
    pub mpaa: Option<&'a str>,
    pub country: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub status: Option<&'a str>,
    pub total_episodes: Option<i32>,
    pub duration: Option<i32>,                     // 平均集时长（分钟）
    pub view_count: Option<i64>,                   // 总播放量
    pub like_count: Option<i64>,                   // 总点赞数
    pub category: i32,                             // 视频分类
    pub tagline: Option<String>,                   // 标语/副标题
    pub set: Option<String>,                       // 系列名称
    pub sorttitle: Option<String>,                 // 排序标题
    pub actors_info: Option<String>,               // 演员信息字符串
    pub staff_info: Option<&'a serde_json::Value>, // 联合投稿成员信息（JSON格式）
    pub cover_url: &'a str,                        // 封面图片URL
    pub fanart_url: Option<&'a str>,               // 背景图片URL
    pub upper_face_url: Option<&'a str>,           // UP主头像URL（用于演员thumb）
    pub season_id: Option<String>,                 // 番剧季度ID
    pub media_id: Option<i64>,                     // 媒体ID
}

impl NFO<'_> {
    pub async fn generate_nfo(self) -> Result<String> {
        let config = crate::config::reload_config();
        let mut buffer = r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
"#
        .as_bytes()
        .to_vec();
        let mut tokio_buffer = BufWriter::new(&mut buffer);
        let writer = Writer::new_with_indent(&mut tokio_buffer, b' ', 4);
        match self {
            NFO::Movie(movie) => {
                Self::write_movie_nfo(writer, movie, &config.nfo_config).await?;
            }
            NFO::TVShow(tvshow) => {
                Self::write_tvshow_nfo(writer, tvshow, &config.nfo_config).await?;
            }
            NFO::Upper(upper) => {
                Self::write_upper_nfo(writer, upper).await?;
            }
            NFO::Episode(episode) => {
                Self::write_episode_nfo(writer, episode, &config.nfo_config).await?;
            }
            NFO::Season(season) => {
                Self::write_season_nfo(writer, season, &config.nfo_config).await?;
            }
        }
        tokio_buffer.flush().await?;
        Ok(String::from_utf8(buffer)?)
    }

    async fn write_movie_nfo(
        mut writer: Writer<&mut BufWriter<&mut Vec<u8>>>,
        movie: Movie<'_>,
        config: &NFOConfig,
    ) -> Result<()> {
        // 验证数据有效性
        if !Self::validate_nfo_data(movie.name, movie.bvid, movie.upper_name) {
            return Err(anyhow::anyhow!(
                "Invalid NFO data: name='{}', bvid='{}', upper_name='{}'",
                movie.name,
                movie.bvid,
                movie.upper_name
            ));
        }

        writer
            .create_element("movie")
            .write_inner_content_async::<_, _, Error>(|writer| async move {
                // 标题信息
                let (display_title, original_title) = if Self::is_bangumi_video(movie.category) {
                    // 对于番剧，尝试提取番剧名称作为主标题
                    if let Some(bangumi_title) = Self::extract_bangumi_title_from_full_name(movie.name) {
                        (bangumi_title, movie.name.to_string())
                    } else {
                        (movie.name.to_string(), movie.original_title.to_string())
                    }
                } else {
                    (movie.name.to_string(), movie.original_title.to_string())
                };

                writer
                    .create_element("title")
                    .write_text_content_async(BytesText::new(&display_title))
                    .await?;
                writer
                    .create_element("originaltitle")
                    .write_text_content_async(BytesText::new(&original_title))
                    .await?;

                // 标语/副标题
                if let Some(ref tagline) = movie.tagline {
                    writer
                        .create_element("tagline")
                        .write_text_content_async(BytesText::new(tagline))
                        .await?;
                }

                // 排序标题
                if let Some(ref sorttitle) = movie.sorttitle {
                    writer
                        .create_element("sorttitle")
                        .write_text_content_async(BytesText::new(sorttitle))
                        .await?;
                } else {
                    // 使用显示标题作为默认排序标题
                    writer
                        .create_element("sorttitle")
                        .write_text_content_async(BytesText::new(&display_title))
                        .await?;
                }

                // 系列信息
                if let Some(ref set_name) = movie.set {
                    writer
                        .create_element("set")
                        .write_inner_content_async::<_, _, Error>(|writer| async move {
                            writer
                                .create_element("name")
                                .write_text_content_async(BytesText::new(set_name))
                                .await?;
                            Ok(writer)
                        })
                        .await?;
                }

                // 评分信息
                if let Some(rating) = movie.user_rating {
                    writer
                        .create_element("userrating")
                        .write_text_content_async(BytesText::new(&rating.to_string()))
                        .await?;
                }

                // 剧情简介
                writer
                    .create_element("plot")
                    .write_cdata_content_async(BytesCData::new(Self::format_plot(movie.bvid, movie.intro)))
                    .await?;
                writer.create_element("outline").write_empty_async().await?;

                // 分级信息
                if let Some(mpaa) = movie.mpaa {
                    writer
                        .create_element("mpaa")
                        .write_text_content_async(BytesText::new(mpaa))
                        .await?;
                }

                // 唯一标识符
                writer
                    .create_element("uniqueid")
                    .with_attribute(("type", "bilibili"))
                    .with_attribute(("default", "true"))
                    .write_text_content_async(BytesText::new(movie.bvid))
                    .await?;

                // 类型标签
                if let Some(tags) = movie.tags {
                    for tag in tags {
                        writer
                            .create_element("genre")
                            .write_text_content_async(BytesText::new(&tag))
                            .await?;
                    }
                }

                // 为番剧剧场版添加默认类型标签
                if Self::is_bangumi_video(movie.category) {
                    writer
                        .create_element("genre")
                        .write_text_content_async(BytesText::new("动画"))
                        .await?;
                    writer
                        .create_element("genre")
                        .write_text_content_async(BytesText::new("剧场版"))
                        .await?;
                }

                // 国家信息
                if let Some(country) = movie.country {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(country))
                        .await?;
                } else {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(&config.default_country))
                        .await?;
                }

                // 创作人员信息
                if let Some(credits) = movie.credits {
                    writer
                        .create_element("credits")
                        .write_text_content_async(BytesText::new(credits))
                        .await?;
                }

                if let Some(director) = movie.director {
                    writer
                        .create_element("director")
                        .write_text_content_async(BytesText::new(director))
                        .await?;
                }

                // 时间信息
                writer
                    .create_element("year")
                    .write_text_content_async(BytesText::new(&movie.aired.format("%Y").to_string()))
                    .await?;
                writer
                    .create_element("premiered")
                    .write_text_content_async(BytesText::new(&movie.premiered.format("%Y-%m-%d").to_string()))
                    .await?;
                writer
                    .create_element("aired")
                    .write_text_content_async(BytesText::new(&movie.aired.format("%Y-%m-%d").to_string()))
                    .await?;

                // 制作信息
                if let Some(studio) = movie.studio {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(studio))
                        .await?;
                } else {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(&config.default_studio))
                        .await?;
                }

                // 演员信息（优先使用真实演员信息，其次联合投稿staff，最后UP主）
                if config.include_actor_info {
                    // 首先尝试使用番剧演员信息
                    if let Some(ref actors_str) = movie.actors_info {
                        let actors = Self::parse_actors_string(actors_str);
                        for (index, (character, actor)) in actors.iter().enumerate() {
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(actor))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(character))
                                        .await?;
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new(&(index + 1).to_string()))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    } else if let Some(ref staff_info) = movie.staff_info {
                        // 使用联合投稿的staff信息（包含头像）
                        let staff_list = Self::parse_staff_info(staff_info);
                        for (index, (mid, name, title, face)) in staff_list.iter().enumerate() {
                            let mid_value = *mid;
                            let name_clone = name.clone();
                            let title_clone = title.clone();
                            let face_clone = face.clone();
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(&name_clone))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(&title_clone))
                                        .await?;
                                    // 头像URL
                                    if let Some(ref thumb) = face_clone {
                                        if !thumb.is_empty() {
                                            writer
                                                .create_element("thumb")
                                                .write_text_content_async(BytesText::new(thumb))
                                                .await?;
                                        }
                                    }
                                    // 稳定标识：B站空间链接（便于脚本按UID映射）
                                    if mid_value > 0 {
                                        writer
                                            .create_element("profile")
                                            .write_text_content_async(BytesText::new(&format!(
                                                "https://space.bilibili.com/{}",
                                                mid_value
                                            )))
                                            .await?;
                                    }
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new(&(index + 1).to_string()))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    } else {
                        // 备选：使用UP主信息作为创作者
                        let actor_info = Self::get_actor_info(movie.upper_id, movie.upper_name, config);
                        if let Some((actor_name, role_name)) = actor_info {
                            let upper_id = movie.upper_id;
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(&actor_name))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(&role_name))
                                        .await?;
                                    // 头像（如果有）
                                    if let Some(thumb) = movie.upper_face_url {
                                        if !thumb.is_empty() {
                                            writer
                                                .create_element("thumb")
                                                .write_text_content_async(BytesText::new(thumb))
                                                .await?;
                                        }
                                    }
                                    // 稳定标识：B站空间链接（便于脚本按UID映射）
                                    if upper_id > 0 {
                                        writer
                                            .create_element("profile")
                                            .write_text_content_async(BytesText::new(&format!(
                                                "https://space.bilibili.com/{}",
                                                upper_id
                                            )))
                                            .await?;
                                    }
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new("1"))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    }
                }

                // 时长信息
                if let Some(duration) = movie.duration {
                    writer
                        .create_element("runtime")
                        .write_text_content_async(BytesText::new(&duration.to_string()))
                        .await?;
                }

                // B站特有信息作为自定义标签
                if config.include_bilibili_info {
                    if let Some(view_count) = movie.view_count {
                        writer
                            .create_element("tag")
                            .write_text_content_async(BytesText::new(&format!("播放量: {}", view_count)))
                            .await?;
                    }

                    if let Some(like_count) = movie.like_count {
                        writer
                            .create_element("tag")
                            .write_text_content_async(BytesText::new(&format!("点赞数: {}", like_count)))
                            .await?;
                    }
                }

                // 封面图信息
                if !movie.cover_url.is_empty() {
                    writer
                        .create_element("thumb")
                        .write_text_content_async(BytesText::new(movie.cover_url))
                        .await?;
                    // 只有在真正有fanart_url时才添加fanart字段
                    if let Some(fanart_url) = movie.fanart_url {
                        if !fanart_url.is_empty() {
                            writer
                                .create_element("fanart")
                                .write_text_content_async(BytesText::new(fanart_url))
                                .await?;
                        }
                    }
                }

                Ok(writer)
            })
            .await?;
        Ok(())
    }

    async fn write_tvshow_nfo(
        mut writer: Writer<&mut BufWriter<&mut Vec<u8>>>,
        tvshow: TVShow<'_>,
        config: &NFOConfig,
    ) -> Result<()> {
        // 验证数据有效性
        if !Self::validate_nfo_data(tvshow.name, tvshow.bvid, tvshow.upper_name) {
            return Err(anyhow::anyhow!(
                "Invalid NFO data: name='{}', bvid='{}', upper_name='{}'",
                tvshow.name,
                tvshow.bvid,
                tvshow.upper_name
            ));
        }

        writer
            .create_element("tvshow")
            .write_inner_content_async::<_, _, Error>(|writer| async move {
                // 标题信息
                let cfg = crate::config::reload_config();
                let is_bangumi = Self::is_bangumi_video(tvshow.category);
                let mut bangumi_series_name: Option<String> = None;

                let (display_title, original_title) = if is_bangumi {
                    if let Some(bangumi_title) = Self::extract_bangumi_title_from_full_name(tvshow.name) {
                        if cfg.bangumi_use_season_structure {
                            let (series_name, _) = crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                                &bangumi_title,
                                None,
                            );
                            let series_name = crate::utils::bangumi_name_extractor::BangumiNameExtractor::normalize_series_name(&series_name);
                            bangumi_series_name = Some(series_name.clone());
                            (series_name.clone(), series_name)
                        } else {
                            (bangumi_title, tvshow.name.to_string())
                        }
                    } else if cfg.bangumi_use_season_structure {
                        // 兜底：直接对标题做一次“去季”处理
                        let (series_name, _) = crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                            tvshow.name,
                            None,
                        );
                        let series_name = crate::utils::bangumi_name_extractor::BangumiNameExtractor::normalize_series_name(&series_name);
                        bangumi_series_name = Some(series_name.clone());
                        (series_name.clone(), series_name)
                    } else {
                        (tvshow.name.to_string(), tvshow.original_title.to_string())
                    }
                } else {
                    (tvshow.name.to_string(), tvshow.original_title.to_string())
                };

                writer
                    .create_element("title")
                    .write_text_content_async(BytesText::new(&display_title))
                    .await?;
                writer
                    .create_element("originaltitle")
                    .write_text_content_async(BytesText::new(&original_title))
                    .await?;

                // 标语/副标题
                if let Some(ref tagline) = tvshow.tagline {
                    writer
                        .create_element("tagline")
                        .write_text_content_async(BytesText::new(tagline))
                        .await?;
                }

                // 排序标题
                if let Some(ref sorttitle) = tvshow.sorttitle {
                    let sorttitle_normalized = if cfg.bangumi_use_season_structure && is_bangumi {
                        bangumi_series_name.clone().unwrap_or_else(|| {
                            let (series_name, _) = crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                                sorttitle,
                                None,
                            );
                            crate::utils::bangumi_name_extractor::BangumiNameExtractor::normalize_series_name(&series_name)
                        })
                    } else {
                        sorttitle.clone()
                    };
                    writer
                        .create_element("sorttitle")
                        .write_text_content_async(BytesText::new(&sorttitle_normalized))
                        .await?;
                } else {
                    // 使用显示标题作为默认排序标题
                    let sort_title_to_write = if cfg.bangumi_use_season_structure && is_bangumi {
                        bangumi_series_name.clone().unwrap_or_else(|| display_title.clone())
                    } else {
                        display_title.clone()
                    };
                    writer
                        .create_element("sorttitle")
                        .write_text_content_async(BytesText::new(&sort_title_to_write))
                        .await?;
                }

                // 系列信息
                if let Some(ref set_name) = tvshow.set {
                    writer
                        .create_element("set")
                        .write_inner_content_async::<_, _, Error>(|writer| async move {
                            writer
                                .create_element("name")
                                .write_text_content_async(BytesText::new(set_name))
                                .await?;
                            Ok(writer)
                        })
                        .await?;
                }

                // 剧情简介
                writer
                    .create_element("plot")
                    .write_cdata_content_async(BytesCData::new(Self::format_plot(tvshow.bvid, tvshow.intro)))
                    .await?;
                writer.create_element("outline").write_empty_async().await?;

                // 评分信息
                if let Some(rating) = tvshow.user_rating {
                    writer
                        .create_element("userrating")
                        .write_text_content_async(BytesText::new(&rating.to_string()))
                        .await?;
                }

                // 分级信息
                if let Some(mpaa) = tvshow.mpaa {
                    writer
                        .create_element("mpaa")
                        .write_text_content_async(BytesText::new(mpaa))
                        .await?;
                }

                // 唯一标识符
                writer
                    .create_element("uniqueid")
                    .with_attribute(("type", "bilibili"))
                    .with_attribute(("default", "true"))
                    .write_text_content_async(BytesText::new(tvshow.bvid))
                    .await?;

                // 添加番剧季度ID作为额外的uniqueid
                if let Some(ref season_id) = tvshow.season_id {
                    writer
                        .create_element("uniqueid")
                        .with_attribute(("type", "bilibili_season"))
                        .write_text_content_async(BytesText::new(season_id))
                        .await?;
                }

                // 添加媒体ID作为额外的uniqueid
                if let Some(media_id) = tvshow.media_id {
                    writer
                        .create_element("uniqueid")
                        .with_attribute(("type", "bilibili_media"))
                        .write_text_content_async(BytesText::new(&media_id.to_string()))
                        .await?;
                }

                // 类型标签
                if let Some(tags) = tvshow.tags {
                    for tag in tags {
                        writer
                            .create_element("genre")
                            .write_text_content_async(BytesText::new(&tag))
                            .await?;
                    }
                }

                // 国家信息
                if let Some(country) = tvshow.country {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(country))
                        .await?;
                } else {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(&config.default_country))
                        .await?;
                }

                // 播出状态
                if let Some(status) = tvshow.status {
                    writer
                        .create_element("status")
                        .write_text_content_async(BytesText::new(status))
                        .await?;
                } else {
                    writer
                        .create_element("status")
                        .write_text_content_async(BytesText::new(&config.default_tvshow_status))
                        .await?;
                }

                // 季数和集数信息
                if let Some(total_seasons) = tvshow.total_seasons {
                    writer
                        .create_element("totalseasons")
                        .write_text_content_async(BytesText::new(&total_seasons.to_string()))
                        .await?;
                }

                if let Some(total_episodes) = tvshow.total_episodes {
                    writer
                        .create_element("totalepisodes")
                        .write_text_content_async(BytesText::new(&total_episodes.to_string()))
                        .await?;
                }

                // 时间信息
                writer
                    .create_element("year")
                    .write_text_content_async(BytesText::new(&tvshow.aired.format("%Y").to_string()))
                    .await?;
                writer
                    .create_element("premiered")
                    .write_text_content_async(BytesText::new(&tvshow.premiered.format("%Y-%m-%d").to_string()))
                    .await?;
                writer
                    .create_element("aired")
                    .write_text_content_async(BytesText::new(&tvshow.aired.format("%Y-%m-%d").to_string()))
                    .await?;

                // 制作信息
                if let Some(studio) = tvshow.studio {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(studio))
                        .await?;
                } else {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(&config.default_studio))
                        .await?;
                }

                // 演员信息（优先使用真实演员信息，其次联合投稿staff，最后UP主）
                if config.include_actor_info {
                    // 首先尝试使用番剧演员信息
                    if let Some(ref actors_str) = tvshow.actors_info {
                        let actors = Self::parse_actors_string(actors_str);
                        for (index, (character, actor)) in actors.iter().enumerate() {
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(actor))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(character))
                                        .await?;
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new(&(index + 1).to_string()))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    } else if let Some(ref staff_info) = tvshow.staff_info {
                        // 使用联合投稿的staff信息（包含头像）
                        let staff_list = Self::parse_staff_info(staff_info);
                        for (index, (mid, name, title, face)) in staff_list.iter().enumerate() {
                            let mid_value = *mid;
                            let name_clone = name.clone();
                            let title_clone = title.clone();
                            let face_clone = face.clone();
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(&name_clone))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(&title_clone))
                                        .await?;
                                    // 头像URL
                                    if let Some(ref thumb) = face_clone {
                                        if !thumb.is_empty() {
                                            writer
                                                .create_element("thumb")
                                                .write_text_content_async(BytesText::new(thumb))
                                                .await?;
                                        }
                                    }
                                    // 稳定标识：B站空间链接（便于脚本按UID映射）
                                    if mid_value > 0 {
                                        writer
                                            .create_element("profile")
                                            .write_text_content_async(BytesText::new(&format!(
                                                "https://space.bilibili.com/{}",
                                                mid_value
                                            )))
                                            .await?;
                                    }
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new(&(index + 1).to_string()))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    } else {
                        // 备选：使用UP主信息作为创作者
                        let actor_info = Self::get_actor_info(tvshow.upper_id, tvshow.upper_name, config);
                        if let Some((actor_name, role_name)) = actor_info {
                            let upper_id = tvshow.upper_id;
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(&actor_name))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(&role_name))
                                        .await?;
                                    // 头像（如果有）
                                    if let Some(thumb) = tvshow.upper_face_url {
                                        if !thumb.is_empty() {
                                            writer
                                                .create_element("thumb")
                                                .write_text_content_async(BytesText::new(thumb))
                                                .await?;
                                        }
                                    }
                                    // 稳定标识：B站空间链接（便于脚本按UID映射）
                                    if upper_id > 0 {
                                        writer
                                            .create_element("profile")
                                            .write_text_content_async(BytesText::new(&format!(
                                                "https://space.bilibili.com/{}",
                                                upper_id
                                            )))
                                            .await?;
                                    }
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new("1"))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    }
                }

                // 时长信息
                if let Some(duration) = tvshow.duration {
                    writer
                        .create_element("runtime")
                        .write_text_content_async(BytesText::new(&duration.to_string()))
                        .await?;
                }

                // B站特有信息作为自定义标签
                if config.include_bilibili_info {
                    if let Some(view_count) = tvshow.view_count {
                        writer
                            .create_element("tag")
                            .write_text_content_async(BytesText::new(&format!("播放量: {}", view_count)))
                            .await?;
                    }

                    if let Some(like_count) = tvshow.like_count {
                        writer
                            .create_element("tag")
                            .write_text_content_async(BytesText::new(&format!("点赞数: {}", like_count)))
                            .await?;
                    }
                }

                // 封面图信息
                if !tvshow.cover_url.is_empty() {
                    writer
                        .create_element("thumb")
                        .write_text_content_async(BytesText::new(tvshow.cover_url))
                        .await?;
                    // 只有在真正有fanart_url时才添加fanart字段
                    if let Some(fanart_url) = tvshow.fanart_url {
                        if !fanart_url.is_empty() {
                            writer
                                .create_element("fanart")
                                .write_text_content_async(BytesText::new(fanart_url))
                                .await?;
                        }
                    }
                }

                Ok(writer)
            })
            .await?;
        Ok(())
    }

    async fn write_upper_nfo(mut writer: Writer<&mut BufWriter<&mut Vec<u8>>>, upper: Upper) -> Result<()> {
        writer
            .create_element("person")
            .write_inner_content_async::<_, _, Error>(|writer| async move {
                writer.create_element("plot").write_empty_async().await?;
                writer.create_element("outline").write_empty_async().await?;
                writer
                    .create_element("lockdata")
                    .write_text_content_async(BytesText::new("false"))
                    .await?;
                writer
                    .create_element("dateadded")
                    .write_text_content_async(BytesText::new(&upper.pubtime.format("%Y-%m-%d %H:%M:%S").to_string()))
                    .await?;
                writer
                    .create_element("title")
                    .write_text_content_async(BytesText::new(&upper.upper_name))
                    .await?;
                writer
                    .create_element("sorttitle")
                    .write_text_content_async(BytesText::new(&upper.upper_name))
                    .await?;
                // 记录UP主的UID作为唯一标识
                writer
                    .create_element("uniqueid")
                    .with_attribute(("type", "bilibili_uid"))
                    .with_attribute(("default", "true"))
                    .write_text_content_async(BytesText::new(&upper.upper_id))
                    .await?;
                Ok(writer)
            })
            .await?;
        Ok(())
    }

    async fn write_episode_nfo(
        mut writer: Writer<&mut BufWriter<&mut Vec<u8>>>,
        episode: Episode<'_>,
        config: &NFOConfig,
    ) -> Result<()> {
        writer
            .create_element("episodedetails")
            .write_inner_content_async::<_, _, Error>(|writer| async move {
                // 标题信息
                writer
                    .create_element("title")
                    .write_text_content_async(BytesText::new(episode.name))
                    .await?;

                // 从标题中清理语言标识，作为原始标题
                let binding = episode
                    .original_title
                    .replace("-中配", "")
                    .replace("-日配", "")
                    .replace("-国语", "")
                    .replace("-粤语", "");
                let cleaned_original_title = binding.trim();

                writer
                    .create_element("originaltitle")
                    .write_text_content_async(BytesText::new(cleaned_original_title))
                    .await?;

                // 剧情简介
                if let Some(plot) = episode.plot {
                    writer
                        .create_element("plot")
                        .write_cdata_content_async(BytesCData::new(plot))
                        .await?;
                } else {
                    writer.create_element("plot").write_empty_async().await?;
                }
                writer.create_element("outline").write_empty_async().await?;

                // 季集信息
                writer
                    .create_element("season")
                    .write_text_content_async(BytesText::new(&episode.season.to_string()))
                    .await?;
                writer
                    .create_element("episode")
                    .write_text_content_async(BytesText::new(&episode.episode_number.to_string()))
                    .await?;

                // 唯一标识符：
                // - 多P场景使用 "BVID-pPID" 作为分集级唯一标识，避免同一BVID下多个分P冲突
                // - 兜底回退到 pid
                let unique_id =
                    if episode.bvid.starts_with("BV") && episode.bvid.len() > 10 && episode.bvid != "BV0000000000" {
                        if !episode.pid.is_empty() && episode.pid != "0" {
                            format!("{}-p{}", episode.bvid, episode.pid)
                        } else {
                            episode.bvid.to_string()
                        }
                    } else {
                        episode.pid.clone()
                    };
                writer
                    .create_element("uniqueid")
                    .with_attribute(("type", "bilibili"))
                    .with_attribute(("default", "true"))
                    .write_text_content_async(BytesText::new(&unique_id))
                    .await?;

                // 类型标签
                if let Some(ref genres) = episode.genres {
                    for genre in genres {
                        writer
                            .create_element("genre")
                            .write_text_content_async(BytesText::new(genre))
                            .await?;
                    }
                }

                // 为番剧添加默认类型标签
                if Self::is_bangumi_video(episode.category) {
                    writer
                        .create_element("genre")
                        .write_text_content_async(BytesText::new("动画"))
                        .await?;
                }

                // 国家信息
                if let Some(country) = episode.country {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(country))
                        .await?;
                } else {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(&config.default_country))
                        .await?;
                }

                // 分级信息
                if let Some(mpaa) = episode.mpaa {
                    writer
                        .create_element("mpaa")
                        .write_text_content_async(BytesText::new(mpaa))
                        .await?;
                } else {
                    // 番剧默认分级为PG
                    writer
                        .create_element("mpaa")
                        .write_text_content_async(BytesText::new("PG"))
                        .await?;
                }

                // 制作工作室
                if let Some(studio) = episode.studio {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(studio))
                        .await?;
                } else {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(&config.default_studio))
                        .await?;
                }

                // 播出时间
                if let Some(aired) = episode.aired {
                    writer
                        .create_element("aired")
                        .write_text_content_async(BytesText::new(&aired.format("%Y-%m-%d").to_string()))
                        .await?;
                }

                // 时长信息
                if let Some(duration) = episode.duration {
                    writer
                        .create_element("runtime")
                        .write_text_content_async(BytesText::new(&duration.to_string()))
                        .await?;
                }

                // 评分信息
                if let Some(rating) = episode.user_rating {
                    writer
                        .create_element("userrating")
                        .write_text_content_async(BytesText::new(&rating.to_string()))
                        .await?;
                }

                // 创作人员信息
                if let Some(director) = episode.director {
                    writer
                        .create_element("director")
                        .write_text_content_async(BytesText::new(director))
                        .await?;
                }

                if let Some(credits) = episode.credits {
                    writer
                        .create_element("credits")
                        .write_text_content_async(BytesText::new(credits))
                        .await?;
                }

                // 缩略图（本地文件路径优先）
                if let Some(thumb_url) = episode.thumb_url {
                    writer
                        .create_element("thumb")
                        .write_text_content_async(BytesText::new(thumb_url))
                        .await?;
                }

                // 背景图（本地文件路径优先）
                if let Some(fanart_url) = episode.fanart_url {
                    writer
                        .create_element("fanart")
                        .write_text_content_async(BytesText::new(fanart_url))
                        .await?;
                }

                Ok(writer)
            })
            .await?;
        Ok(())
    }

    async fn write_season_nfo(
        mut writer: Writer<&mut BufWriter<&mut Vec<u8>>>,
        season: Season<'_>,
        config: &NFOConfig,
    ) -> Result<()> {
        // 验证数据有效性
        if !Self::validate_nfo_data(season.name, season.bvid, season.upper_name) {
            return Err(anyhow::anyhow!(
                "Invalid NFO data: name='{}', bvid='{}', upper_name='{}'",
                season.name,
                season.bvid,
                season.upper_name
            ));
        }

        writer
            .create_element("season")
            .write_inner_content_async::<_, _, Error>(|writer| async move {
                // 标题信息 - 根据Emby标准，Season应该显示纯季度标题（如"第二季"）
                let (display_title, original_title) = if Self::is_bangumi_video(season.category) {
                    // 尝试提取纯季度标题
                    if let Some(season_title) = Self::extract_season_title_from_full_name(season.name) {
                        (season_title, season.name.to_string())
                    } else {
                        // 如果提取失败，使用完整名称
                        (season.name.to_string(), season.original_title.to_string())
                    }
                } else {
                    // 非番剧合集使用标准季度标题，避免媒体库把季当成单个视频
                    if let Some(ref set_name) = season.set {
                        (format!("第{}季", season.season_number.max(1)), set_name.clone())
                    } else {
                        (season.name.to_string(), season.original_title.to_string())
                    }
                };

                writer
                    .create_element("title")
                    .write_text_content_async(BytesText::new(&display_title))
                    .await?;
                writer
                    .create_element("originaltitle")
                    .write_text_content_async(BytesText::new(&original_title))
                    .await?;

                // 季数信息
                writer
                    .create_element("seasonnumber")
                    .write_text_content_async(BytesText::new(&season.season_number.to_string()))
                    .await?;

                // 标语/副标题
                if let Some(ref tagline) = season.tagline {
                    writer
                        .create_element("tagline")
                        .write_text_content_async(BytesText::new(tagline))
                        .await?;
                }

                // 排序标题
                if let Some(ref sorttitle) = season.sorttitle {
                    writer
                        .create_element("sorttitle")
                        .write_text_content_async(BytesText::new(sorttitle))
                        .await?;
                } else {
                    // 使用显示标题作为默认排序标题
                    writer
                        .create_element("sorttitle")
                        .write_text_content_async(BytesText::new(&display_title))
                        .await?;
                }

                // 系列信息
                if let Some(ref set_name) = season.set {
                    writer
                        .create_element("set")
                        .write_inner_content_async::<_, _, Error>(|writer| async move {
                            writer
                                .create_element("name")
                                .write_text_content_async(BytesText::new(set_name))
                                .await?;
                            Ok(writer)
                        })
                        .await?;
                }

                // 剧情简介 - 为Season添加季度特定的前缀
                let season_plot = if Self::is_bangumi_video(season.category) {
                    if let Some(season_title) = Self::extract_season_title_from_full_name(season.name) {
                        format!("【{}】{}", season_title, Self::format_plot(season.bvid, season.intro))
                    } else {
                        Self::format_plot(season.bvid, season.intro)
                    }
                } else {
                    Self::format_plot(season.bvid, season.intro)
                };
                writer
                    .create_element("plot")
                    .write_cdata_content_async(BytesCData::new(season_plot))
                    .await?;
                writer.create_element("outline").write_empty_async().await?;

                // 评分信息
                if let Some(rating) = season.user_rating {
                    writer
                        .create_element("userrating")
                        .write_text_content_async(BytesText::new(&rating.to_string()))
                        .await?;
                }

                // 分级信息
                if let Some(mpaa) = season.mpaa {
                    writer
                        .create_element("mpaa")
                        .write_text_content_async(BytesText::new(mpaa))
                        .await?;
                }

                // 唯一标识符
                writer
                    .create_element("uniqueid")
                    .with_attribute(("type", "bilibili"))
                    .with_attribute(("default", "true"))
                    .write_text_content_async(BytesText::new(season.bvid))
                    .await?;

                // 添加番剧季度ID作为额外的uniqueid
                if let Some(ref season_id) = season.season_id {
                    writer
                        .create_element("uniqueid")
                        .with_attribute(("type", "bilibili_season"))
                        .write_text_content_async(BytesText::new(season_id))
                        .await?;
                }

                // 添加媒体ID作为额外的uniqueid
                if let Some(media_id) = season.media_id {
                    writer
                        .create_element("uniqueid")
                        .with_attribute(("type", "bilibili_media"))
                        .write_text_content_async(BytesText::new(&media_id.to_string()))
                        .await?;
                }

                // 类型标签
                if let Some(tags) = season.tags {
                    for tag in tags {
                        writer
                            .create_element("genre")
                            .write_text_content_async(BytesText::new(&tag))
                            .await?;
                    }
                }

                // 国家信息
                if let Some(country) = season.country {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(country))
                        .await?;
                } else {
                    writer
                        .create_element("country")
                        .write_text_content_async(BytesText::new(&config.default_country))
                        .await?;
                }

                // 播出状态
                if let Some(status) = season.status {
                    writer
                        .create_element("status")
                        .write_text_content_async(BytesText::new(status))
                        .await?;
                } else {
                    writer
                        .create_element("status")
                        .write_text_content_async(BytesText::new(&config.default_tvshow_status))
                        .await?;
                }

                // 集数信息
                if let Some(total_episodes) = season.total_episodes {
                    writer
                        .create_element("totalepisodes")
                        .write_text_content_async(BytesText::new(&total_episodes.to_string()))
                        .await?;
                }

                // 时间信息
                writer
                    .create_element("year")
                    .write_text_content_async(BytesText::new(&season.aired.format("%Y").to_string()))
                    .await?;
                writer
                    .create_element("premiered")
                    .write_text_content_async(BytesText::new(&season.premiered.format("%Y-%m-%d").to_string()))
                    .await?;
                writer
                    .create_element("aired")
                    .write_text_content_async(BytesText::new(&season.aired.format("%Y-%m-%d").to_string()))
                    .await?;

                // 制作信息
                if let Some(studio) = season.studio {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(studio))
                        .await?;
                } else {
                    writer
                        .create_element("studio")
                        .write_text_content_async(BytesText::new(&config.default_studio))
                        .await?;
                }

                // 演员信息（优先使用真实演员信息，其次联合投稿staff，最后UP主）
                if config.include_actor_info {
                    // 首先尝试使用番剧演员信息
                    if let Some(ref actors_str) = season.actors_info {
                        let actors = Self::parse_actors_string(actors_str);
                        for (index, (character, actor)) in actors.iter().enumerate() {
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(actor))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(character))
                                        .await?;
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new(&(index + 1).to_string()))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    } else if let Some(ref staff_info) = season.staff_info {
                        // 使用联合投稿的staff信息（包含头像）
                        let staff_list = Self::parse_staff_info(staff_info);
                        for (index, (mid, name, title, face)) in staff_list.iter().enumerate() {
                            let mid_value = *mid;
                            let name_clone = name.clone();
                            let title_clone = title.clone();
                            let face_clone = face.clone();
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(&name_clone))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(&title_clone))
                                        .await?;
                                    // 头像URL
                                    if let Some(ref thumb) = face_clone {
                                        if !thumb.is_empty() {
                                            writer
                                                .create_element("thumb")
                                                .write_text_content_async(BytesText::new(thumb))
                                                .await?;
                                        }
                                    }
                                    // 稳定标识：B站空间链接（便于脚本按UID映射）
                                    if mid_value > 0 {
                                        writer
                                            .create_element("profile")
                                            .write_text_content_async(BytesText::new(&format!(
                                                "https://space.bilibili.com/{}",
                                                mid_value
                                            )))
                                            .await?;
                                    }
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new(&(index + 1).to_string()))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    } else {
                        // 备选：使用UP主信息作为创作者
                        let actor_info = Self::get_actor_info(season.upper_id, season.upper_name, config);
                        if let Some((actor_name, role_name)) = actor_info {
                            let upper_id = season.upper_id;
                            writer
                                .create_element("actor")
                                .write_inner_content_async::<_, _, Error>(|writer| async move {
                                    writer
                                        .create_element("name")
                                        .write_text_content_async(BytesText::new(&actor_name))
                                        .await?;
                                    writer
                                        .create_element("role")
                                        .write_text_content_async(BytesText::new(&role_name))
                                        .await?;
                                    // 头像（如果有）
                                    if let Some(thumb) = season.upper_face_url {
                                        if !thumb.is_empty() {
                                            writer
                                                .create_element("thumb")
                                                .write_text_content_async(BytesText::new(thumb))
                                                .await?;
                                        }
                                    }
                                    // 稳定标识：B站空间链接（便于脚本按UID映射）
                                    if upper_id > 0 {
                                        writer
                                            .create_element("profile")
                                            .write_text_content_async(BytesText::new(&format!(
                                                "https://space.bilibili.com/{}",
                                                upper_id
                                            )))
                                            .await?;
                                    }
                                    writer
                                        .create_element("order")
                                        .write_text_content_async(BytesText::new("1"))
                                        .await?;
                                    Ok(writer)
                                })
                                .await?;
                        }
                    }
                }

                // 时长信息
                if let Some(duration) = season.duration {
                    writer
                        .create_element("runtime")
                        .write_text_content_async(BytesText::new(&duration.to_string()))
                        .await?;
                }

                // B站特有信息作为自定义标签
                if config.include_bilibili_info {
                    if let Some(view_count) = season.view_count {
                        writer
                            .create_element("tag")
                            .write_text_content_async(BytesText::new(&format!("播放量: {}", view_count)))
                            .await?;
                    }

                    if let Some(like_count) = season.like_count {
                        writer
                            .create_element("tag")
                            .write_text_content_async(BytesText::new(&format!("点赞数: {}", like_count)))
                            .await?;
                    }
                }

                // 封面图信息
                if !season.cover_url.is_empty() {
                    writer
                        .create_element("thumb")
                        .write_text_content_async(BytesText::new(season.cover_url))
                        .await?;
                    // 只有在真正有fanart_url时才添加fanart字段
                    if let Some(fanart_url) = season.fanart_url {
                        if !fanart_url.is_empty() {
                            writer
                                .create_element("fanart")
                                .write_text_content_async(BytesText::new(fanart_url))
                                .await?;
                        }
                    }
                }

                Ok(writer)
            })
            .await?;
        Ok(())
    }

    #[inline]
    fn format_plot(bvid: &str, intro: &str) -> String {
        format!(
            r#"原始视频：<a href="https://www.bilibili.com/video/{}/">{}</a><br/><br/>{}"#,
            bvid, bvid, intro,
        )
    }

    /// 检测是否为番剧视频（基于 category 字段）
    fn is_bangumi_video(category: i32) -> bool {
        category == 1
    }

    /// 从完整标题中提取纯季度标题（如"第二季"）
    fn extract_season_title_from_full_name(full_name: &str) -> Option<String> {
        // 匹配 "番剧名称第X季" 格式，提取季度部分
        let pattern = regex::Regex::new(r".+?(第[一二三四五六七八九十\d]+季)").unwrap();
        if let Some(caps) = pattern.captures(full_name) {
            return Some(caps[1].to_string());
        }
        None
    }

    /// 从完整标题中提取番剧名称
    fn extract_bangumi_title_from_full_name(full_name: &str) -> Option<String> {
        // 匹配 "《番剧名称》第X话/集 副标题" 格式
        let pattern1 = regex::Regex::new(r"《([^》]+)》").unwrap();
        if let Some(caps) = pattern1.captures(full_name) {
            return Some(caps[1].to_string());
        }

        // 匹配 "番剧名称 第X话/集" 格式
        let pattern2 = regex::Regex::new(r"^([^第]+)\s*第\d+[话集]").unwrap();
        if let Some(caps) = pattern2.captures(full_name) {
            return Some(caps[1].trim().to_string());
        }

        // 匹配番剧标题后跟描述性文本（如"柯南剧场版开山之作"）
        let pattern3 = regex::Regex::new(r"《([^》]+)》(.+)").unwrap();
        if let Some(caps) = pattern3.captures(full_name) {
            let title = caps[1].trim();
            let subtitle = caps[2].trim();
            // 如果副标题不是集数信息，则只返回主标题
            if !subtitle.contains("第") && !subtitle.contains("话") && !subtitle.contains("集") {
                return Some(title.to_string());
            }
        }

        // 匹配 "番剧名称第X季" 格式，用于TVShow标题清理
        let pattern4 = regex::Regex::new(r"^(.+?)第[一二三四五六七八九十\d]+季$").unwrap();
        if let Some(caps) = pattern4.captures(full_name) {
            return Some(caps[1].trim().to_string());
        }

        None
    }

    /// 计算番剧的实际总季数（经验推断）
    /// 通过分析标题中的季度信息来估算总季数。
    fn calculate_total_seasons_from_title(title: &str) -> i32 {
        // 如果标题包含季度信息，尝试提取季度数字
        if let Some(season_match) = regex::Regex::new(r"第([一二三四五六七八九十\d]+)季")
            .unwrap()
            .captures(title)
        {
            let season_str = &season_match[1];

            // 处理中文数字转换并返回当前检测到的季度作为总季数的估计
            // 这是基于标题的最佳猜测
            return match season_str {
                "一" => 1,
                "二" => 2,
                "三" => 3,
                "四" => 4,
                "五" => 5,
                "六" => 6,
                "七" => 7,
                "八" => 8,
                "九" => 9,
                "十" => 10,
                _ => season_str.parse::<i32>().unwrap_or(1),
            };
        }

        // 没有季度信息，假设为单季
        1
    }

    /// 从share_copy或标题中提取副标题信息
    fn extract_subtitle_from_share_copy(share_copy: &str) -> Option<String> {
        // 匹配 "《番剧名称》副标题" 格式，提取副标题
        let pattern = regex::Regex::new(r"《[^》]+》\s*(.+)").unwrap();
        if let Some(caps) = pattern.captures(share_copy) {
            let subtitle = caps[1].trim();
            // 过滤掉一些常见的无意义副标题
            if !subtitle.is_empty() &&
               !subtitle.contains("第") && 
               !subtitle.contains("话") && 
               !subtitle.contains("集") &&
               subtitle != "日配" &&  // 过滤掉语言标识
               subtitle != "中配" &&
               subtitle != "国语" &&
               subtitle != "粤语" &&
               subtitle.len() > 2
            {
                return Some(subtitle.to_string());
            }
        }
        None
    }

    /// 验证NFO数据的有效性
    fn validate_nfo_data(name: &str, bvid: &str, _upper_name: &str) -> bool {
        // 检查基本字段是否有效
        !name.trim().is_empty() && !bvid.trim().is_empty() && bvid.starts_with("BV") && bvid.len() >= 10
        // BV号最少10位
    }

    /// 根据配置策略获取演员信息（返回演员名称和角色名称）
    ///
    /// 设计目标：
    /// - `actor.name` 使用 UP 主昵称（媒体库按演员分类更符合直觉，避免出现纯数字导致漏归类）；
    /// - `actor.role` 固定为「UP主」；
    /// - 当昵称为空时按策略处理（占位/默认/跳过）。
    fn get_actor_info(upper_id: i64, upper_name: &str, config: &NFOConfig) -> Option<(String, String)> {
        let trimmed_name = upper_name.trim();

        let name = if !trimmed_name.is_empty() {
            trimmed_name.to_string()
        } else {
            match config.empty_upper_strategy {
                EmptyUpperStrategy::Skip => return None,
                EmptyUpperStrategy::Placeholder => config.empty_upper_placeholder.clone(),
                EmptyUpperStrategy::Default => config.empty_upper_default_name.clone(),
            }
        };

        // upper_id 这里不再写入 actor.name，避免媒体库把纯数字当作演员名导致漏归类。
        // 如果后续需要稳定标识，可另行在 NFO 中扩展字段或通过 uniqueid/bvid 做映射。
        let _ = upper_id;

        Some((name, "UP主".to_string()))
    }

    /// 解析演员信息字符串，返回 (角色名, 声优名) 的向量
    /// 输入格式如："江户川柯南：高山南\n毛利兰：山崎和佳奈\n毛利小五郎：神谷明"
    fn parse_actors_string(actors_str: &str) -> Vec<(String, String)> {
        let mut actors = Vec::new();

        for line in actors_str.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // 支持多种分隔符：全角冒号、半角冒号
            if let Some(pos) = line.find('：') {
                let character = line[..pos].trim().to_string();
                let actor = line[pos + 3..].trim().to_string(); // 全角冒号占3字节
                if !character.is_empty() && !actor.is_empty() {
                    actors.push((character, actor));
                }
            } else if let Some(pos) = line.find(':') {
                let character = line[..pos].trim().to_string();
                let actor = line[pos + 1..].trim().to_string();
                if !character.is_empty() && !actor.is_empty() {
                    actors.push((character, actor));
                }
            } else {
                // 如果没有分隔符，可能是单独的演员名，作为通用演员处理
                if !line.is_empty() {
                    actors.push(("演员".to_string(), line.to_string()));
                }
            }
        }

        actors
    }

    /// 解析 staff_info JSON，返回 (mid, 名称, 职位, 头像URL) 的向量
    /// staff_info 格式: [{"mid": 123, "title": "UP主", "name": "用户名", "face": "头像URL"}, ...]
    fn parse_staff_info(staff_info: &serde_json::Value) -> Vec<(i64, String, String, Option<String>)> {
        let mut staff_list = Vec::new();

        if let Some(arr) = staff_info.as_array() {
            for item in arr {
                let mid = item.get("mid").and_then(|v| v.as_i64()).unwrap_or(0);
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("参与者");
                let face = item.get("face").and_then(|v| v.as_str()).map(|s| s.to_string());

                if !name.is_empty() {
                    staff_list.push((mid, name.to_string(), title.to_string(), face));
                }
            }
        }

        staff_list
    }
}

impl<'a> From<&'a video::Model> for Movie<'a> {
    fn from(video: &'a video::Model) -> Self {
        // 使用动态配置而非静态CONFIG
        let config = crate::config::reload_config();

        // 对于番剧影视类型（show_season_type=2），使用share_copy作为标题
        // 其他类型继续使用video.name
        let nfo_title = if video.show_season_type == Some(2) {
            video.share_copy.as_deref().unwrap_or(&video.name)
        } else {
            &video.name
        };

        let aired_time = match config.nfo_config.time_type {
            NFOTimeType::FavTime => video.favtime,
            NFOTimeType::PubTime => video.pubtime,
        };

        // 提取标语/副标题
        let tagline = if video.show_season_type == Some(2) {
            video
                .share_copy
                .as_ref()
                .and_then(|sc| NFO::extract_subtitle_from_share_copy(sc))
        } else {
            None
        };

        // 生成排序标题（去除特殊字符，便于排序）
        let sorttitle = Some({
            // 对于番剧，使用提取的系列名称；否则使用原标题
            if NFO::is_bangumi_video(video.category) {
                if let Some(bangumi_title) = NFO::extract_bangumi_title_from_full_name(nfo_title) {
                    bangumi_title
                } else {
                    // 如果提取失败，手动清理标题
                    nfo_title
                        .replace("《", "")
                        .replace("》", "")
                        .split_whitespace()
                        .next()
                        .unwrap_or(nfo_title)
                        .to_string()
                }
            } else {
                nfo_title.to_string()
            }
        });

        // 对于番剧，尝试提取系列名称
        let set_name = if NFO::is_bangumi_video(video.category) {
            NFO::extract_bangumi_title_from_full_name(nfo_title)
        } else {
            None
        };

        Self {
            name: nfo_title,
            original_title: &video.name,
            intro: &video.intro,
            bvid: &video.bvid,
            upper_id: video.upper_id,
            upper_name: &video.upper_name,
            aired: aired_time,
            premiered: aired_time,
            tags: video
                .tags
                .as_ref()
                .and_then(|tags| serde_json::from_value(tags.clone()).ok()),
            user_rating: None, // B站没有评分系统
            mpaa: None,        // 使用默认分级
            country: None,     // 使用默认值（中国）
            studio: None,      // 使用默认值（哔哩哔哩）
            director: None,    // UP主信息在actor中
            credits: None,     // UP主信息在actor中
            duration: None,    // video模型中没有duration字段
            view_count: None,  // video模型中没有view_count字段
            like_count: None,  // video模型中没有like_count字段
            category: video.category,
            tagline,
            set: set_name,
            sorttitle,
            actors_info: video.actors.clone(),
            staff_info: video.staff_info.as_ref(),
            cover_url: &video.cover,
            fanart_url: None, // Movie暂不单独设置fanart URL
            upper_face_url: if !video.upper_face.is_empty() {
                Some(&video.upper_face)
            } else {
                None
            },
        }
    }
}

impl<'a> From<&'a video::Model> for TVShow<'a> {
    fn from(video: &'a video::Model) -> Self {
        // 使用动态配置而非静态CONFIG
        let config = crate::config::reload_config();

        // 对于番剧影视类型（show_season_type=2），使用share_copy作为标题
        // 其他类型继续使用video.name
        let nfo_title = if video.show_season_type == Some(2) {
            video.share_copy.as_deref().unwrap_or(&video.name)
        } else {
            &video.name
        };

        let aired_time = match config.nfo_config.time_type {
            NFOTimeType::FavTime => video.favtime,
            NFOTimeType::PubTime => video.pubtime,
        };

        // 提取标语/副标题
        let tagline = if video.show_season_type == Some(2) {
            video
                .share_copy
                .as_ref()
                .and_then(|sc| NFO::extract_subtitle_from_share_copy(sc))
        } else {
            None
        };

        // 生成排序标题（去除特殊字符，便于排序）
        let sorttitle = Some({
            // 对于番剧，使用提取的系列名称；否则使用原标题
            if NFO::is_bangumi_video(video.category) {
                if let Some(bangumi_title) = NFO::extract_bangumi_title_from_full_name(nfo_title) {
                    bangumi_title
                } else {
                    // 如果提取失败，手动清理标题
                    nfo_title
                        .replace("《", "")
                        .replace("》", "")
                        .split_whitespace()
                        .next()
                        .unwrap_or(nfo_title)
                        .to_string()
                }
            } else {
                nfo_title.to_string()
            }
        });

        // 对于番剧，尝试提取系列名称
        let set_name = if NFO::is_bangumi_video(video.category) {
            NFO::extract_bangumi_title_from_full_name(nfo_title)
        } else {
            None
        };

        Self {
            name: nfo_title,
            original_title: &video.name,
            intro: &video.intro,
            bvid: &video.bvid,
            upper_id: video.upper_id,
            upper_name: &video.upper_name,
            aired: aired_time,
            premiered: aired_time,
            tags: video
                .tags
                .as_ref()
                .and_then(|tags| serde_json::from_value(tags.clone()).ok()),
            user_rating: None,          // B站没有评分系统
            mpaa: None,                 // 使用默认分级
            country: None,              // 使用默认值（中国）
            studio: None,               // 使用默认值（哔哩哔哩）
            status: Some("Continuing"), // 默认持续播出状态
            total_seasons: Some(if NFO::is_bangumi_video(video.category) {
                // 优先使用 DB 字段，其次从标题推断
                video
                    .season_number
                    .filter(|n| *n > 0)
                    .unwrap_or_else(|| NFO::calculate_total_seasons_from_title(&video.name))
            } else {
                1
            }),
            total_episodes: None, // 从分页数量推断
            duration: None,       // video模型中没有duration字段
            view_count: None,     // video模型中没有view_count字段
            like_count: None,     // video模型中没有like_count字段
            category: video.category,
            tagline,
            set: set_name,
            sorttitle,
            actors_info: video.actors.clone(),
            staff_info: video.staff_info.as_ref(),
            cover_url: &video.cover,
            fanart_url: None, // 普通视频没有单独的fanart URL
            upper_face_url: if !video.upper_face.is_empty() {
                Some(&video.upper_face)
            } else {
                None
            },
            season_id: None, // 普通视频没有season_id
            media_id: None,  // 普通视频没有media_id
        }
    }
}

impl<'a> TVShow<'a> {
    /// 从视频模型和合集信息创建TVShow，优先使用合集名称和封面
    pub fn from_video_with_collection(
        video: &'a video::Model,
        collection_name: Option<&'a str>,
        collection_cover: Option<&'a str>,
        collection_description: Option<&'a str>,
        season_number: i32,
        total_seasons: Option<i32>,
        total_episodes: Option<i32>,
    ) -> Self {
        // 首先获取基础的TVShow
        let mut tvshow = TVShow::from(video);
        let safe_season = season_number.max(1);

        // 如果提供了合集信息，优先使用合集名称和封面
        if let Some(name) = collection_name {
            tvshow.name = name;
            tvshow.original_title = name;
            tvshow.sorttitle = Some(name.to_string());
        }

        if let Some(cover) = collection_cover {
            tvshow.cover_url = cover;
            tvshow.fanart_url = Some(cover);
        }

        if let Some(description) = collection_description {
            let description = description.trim();
            if !description.is_empty() {
                tvshow.intro = description;
            }
        }

        // 合集级NFO避免使用首个视频标签，减少媒体库错误归类
        tvshow.tags = None;
        tvshow.tagline = None;

        tvshow.total_seasons = Some(total_seasons.unwrap_or(safe_season).max(1));
        tvshow.total_episodes = total_episodes.map(|v| v.max(1));
        if let Some(name) = collection_name {
            tvshow.set = Some(name.to_string());
        }

        tvshow
    }
}

// 带页面数据的转换实现，用于计算总时长
impl<'a> Movie<'a> {
    /// 从视频模型和页面数据创建Movie，包含计算得出的总时长
    pub fn from_video_with_pages(video: &'a video::Model, pages: &[page::Model]) -> Self {
        let mut movie = Movie::from(video);

        // 计算总时长（分钟）
        if !pages.is_empty() {
            let total_duration_seconds: u64 = pages.iter().map(|p| p.duration as u64).sum();
            let total_duration_minutes = (total_duration_seconds / 60) as i32;
            movie.duration = Some(total_duration_minutes);
        }

        movie
    }
}

impl<'a> TVShow<'a> {
    /// 从视频模型和页面数据创建TVShow，包含计算得出的总时长
    #[cfg(test)]
    pub fn from_video_with_pages(video: &'a video::Model, pages: &[page::Model]) -> Self {
        let mut tvshow = TVShow::from(video);

        // 计算总时长（分钟）
        if !pages.is_empty() {
            let total_duration_seconds: u64 = pages.iter().map(|p| p.duration as u64).sum();
            let total_duration_minutes = (total_duration_seconds / 60) as i32;
            tvshow.duration = Some(total_duration_minutes);
        }

        // 对于番剧，total_episodes应该是整个季的集数，而不是当前页面数
        // 这里暂时设为None，避免显示错误的"1集"信息
        tvshow.total_episodes = None;

        tvshow
    }

    /// 从API获取的SeasonInfo创建带有完整元数据的TVShow
    pub fn from_season_info(video: &'a video::Model, season_info: &'a crate::workflow::SeasonInfo) -> Self {
        // 使用动态配置而非静态CONFIG
        let config = crate::config::reload_config();

        // 优先使用API的发布时间，如果没有则使用配置的时间类型
        let aired_time = if let Some(ref publish_time) = season_info.publish_time {
            // 使用统一的时间解析函数
            {
                let fallback_time = match config.nfo_config.time_type {
                    crate::config::NFOTimeType::FavTime => video.favtime,
                    crate::config::NFOTimeType::PubTime => video.pubtime,
                };
                parse_time_string(publish_time).unwrap_or(fallback_time)
            }
        } else {
            // 没有API时间，使用配置的时间类型
            match config.nfo_config.time_type {
                crate::config::NFOTimeType::FavTime => video.favtime,
                crate::config::NFOTimeType::PubTime => video.pubtime,
            }
        };

        // 使用API提供的信息
        let nfo_title = &season_info.title;
        let evaluate = season_info.evaluate.as_deref().unwrap_or(&video.intro);

        // 制作地区处理（使用第一个地区或默认值）
        let country = season_info.areas.first().map(|s| s.as_str());

        // 播出状态
        let status = season_info.status.as_deref();

        // 类型标签
        let genres: Option<Vec<String>> = if !season_info.styles.is_empty() {
            Some(season_info.styles.clone())
        } else {
            // 备选：使用video中的tags
            video
                .tags
                .as_ref()
                .and_then(|tags| serde_json::from_value(tags.clone()).ok())
        };

        // 构建用户评分信息，包含评分人数（作为标签使用）
        let _rating_info = if let (Some(rating), Some(rating_count)) = (season_info.rating, season_info.rating_count) {
            Some(format!("{:.1}分，{}人评价", rating, rating_count))
        } else {
            season_info.rating.map(|r| format!("{:.1}分", r))
        };

        Self {
            name: nfo_title,
            original_title: season_info.alias.as_deref().unwrap_or(&season_info.title),
            intro: evaluate,
            bvid: &video.bvid,
            upper_id: video.upper_id,
            upper_name: &video.upper_name,
            aired: aired_time,
            premiered: aired_time,
            tags: genres,
            user_rating: season_info.rating,
            mpaa: None, // 可以从API的"分级"字段获取，但目前API中没有
            country,
            studio: None, // 可以从制作公司获取，但API中暂无此字段
            status,
            total_seasons: None, // 不生成totalseasons，让Jellyfin自动发现
            total_episodes: season_info.total_episodes,
            duration: None, // 单集平均时长，需要计算
            view_count: season_info.total_views,
            like_count: season_info.total_favorites,
            category: video.category,
            tagline: season_info.alias.as_deref().map(|s| s.to_string()),
            set: if NFO::is_bangumi_video(video.category) {
                NFO::extract_bangumi_title_from_full_name(&season_info.title)
            } else {
                Some(season_info.title.clone())
            }, // 系列名称（清理季度信息）
            sorttitle: Some(season_info.title.clone()),
            actors_info: season_info.actors.clone(),
            staff_info: video.staff_info.as_ref(), // 番剧一般使用actors_info，staff_info作为备选
            cover_url: season_info
                .cover
                .as_deref()
                .or(season_info.horizontal_cover_169.as_deref())
                .or(season_info.horizontal_cover_1610.as_deref())
                .unwrap_or(&video.cover),
            fanart_url: season_info.cover.as_deref().filter(|s| !s.is_empty()),
            upper_face_url: if !video.upper_face.is_empty() {
                Some(&video.upper_face)
            } else {
                None
            },
            // 使用season_id和media_id作为额外的uniqueid（通过扩展字段传递）
            season_id: Some(season_info.season_id.clone()),
            media_id: season_info.media_id,
        }
    }
}

impl<'a> From<&'a video::Model> for Upper {
    fn from(video: &'a video::Model) -> Self {
        Self {
            upper_id: video.upper_id.to_string(),
            upper_name: video.upper_name.clone(),
            pubtime: video.pubtime,
        }
    }
}

impl<'a> From<&'a page::Model> for Episode<'a> {
    fn from(page: &'a page::Model) -> Self {
        Self {
            name: &page.name,
            original_title: &page.name,
            pid: page.pid.to_string(),
            plot: None,                                // 分页没有单独简介
            season: 1,                                 // 默认第一季
            episode_number: page.pid,                  // 使用页面ID作为集数
            aired: None,                               // 分页没有单独播出时间
            duration: Some(page.duration as i32 / 60), // 分页时长转换为分钟
            user_rating: None,                         // 分页没有单独评分
            director: None,                            // 分页没有单独导演信息
            credits: None,                             // 分页没有单独创作人员信息
            bvid: "BV0000000000",                      // 默认BVID
            category: 0,                               // 默认分类
            mpaa: None,                                // 使用默认分级
            country: None,                             // 使用默认国家
            studio: None,                              // 使用默认制作工作室
            genres: None,                              // 无类型标签
            thumb_url: None,                           // 暂不设置本地路径
            fanart_url: None,                          // 暂不设置本地路径
        }
    }
}

impl<'a> Episode<'a> {
    /// 从视频模型和页面模型创建Episode，使用正确的episode_number
    pub fn from_video_and_page(video: &'a video::Model, page: &'a page::Model) -> Self {
        // 判断是否为番剧且启用了Season结构
        let is_bangumi = video.source_type == Some(1);
        let config = crate::config::reload_config();
        let use_unified_season = is_bangumi && config.bangumi_use_season_structure;

        // 启用Season结构时，统一使用season=1；否则使用原始season_number
        let season_number = if use_unified_season {
            1
        } else {
            video.season_number.unwrap_or(1)
        };

        Self {
            name: &page.name,
            original_title: &page.name,
            pid: page.pid.to_string(),
            plot: Some(&video.intro),                                 // 使用视频简介
            season: season_number,                                    // 根据配置使用统一season或原始season_number
            episode_number: video.episode_number.unwrap_or(page.pid), // 使用video的episode_number
            aired: Some(video.pubtime),                               // 使用视频发布时间
            duration: Some(page.duration as i32 / 60),                // 分页时长转换为分钟
            user_rating: None,                                        // 分页没有单独评分
            director: None,                                           // 分页没有单独导演信息
            credits: None,                                            // 分页没有单独创作人员信息
            bvid: &video.bvid,                                        // B站视频ID
            category: video.category,                                 // 视频分类
            mpaa: None,                                               // 使用默认分级（PG）
            country: None,                                            // 使用默认国家
            studio: None,                                             // 使用默认制作工作室
            genres: video
                .tags
                .as_ref()
                .and_then(|tags| serde_json::from_value(tags.clone()).ok()), // 从视频标签提取类型
            thumb_url: None,                                          // 暂不设置本地路径
            fanart_url: None,                                         // 暂不设置本地路径
        }
    }
}

impl<'a> From<&'a video::Model> for Season<'a> {
    fn from(video: &'a video::Model) -> Self {
        // 使用动态配置而非静态CONFIG
        let config = crate::config::reload_config();

        // 对于番剧影视类型（show_season_type=2），使用share_copy作为标题
        // 其他类型继续使用video.name
        let nfo_title = if video.show_season_type == Some(2) {
            video.share_copy.as_deref().unwrap_or(&video.name)
        } else {
            &video.name
        };

        let aired_time = match config.nfo_config.time_type {
            NFOTimeType::FavTime => video.favtime,
            NFOTimeType::PubTime => video.pubtime,
        };

        // 提取标语/副标题
        let tagline = if video.show_season_type == Some(2) {
            video
                .share_copy
                .as_ref()
                .and_then(|sc| NFO::extract_subtitle_from_share_copy(sc))
        } else {
            None
        };

        // 生成排序标题（去除特殊字符，便于排序）
        let sorttitle = Some({
            // 对于番剧，使用提取的系列名称；否则使用原标题
            if NFO::is_bangumi_video(video.category) {
                if let Some(bangumi_title) = NFO::extract_bangumi_title_from_full_name(nfo_title) {
                    bangumi_title
                } else {
                    // 如果提取失败，手动清理标题
                    nfo_title
                        .replace("《", "")
                        .replace("》", "")
                        .split_whitespace()
                        .next()
                        .unwrap_or(nfo_title)
                        .to_string()
                }
            } else {
                nfo_title.to_string()
            }
        });

        // 对于番剧，尝试提取系列名称
        let set_name = if NFO::is_bangumi_video(video.category) {
            NFO::extract_bangumi_title_from_full_name(nfo_title)
        } else {
            None
        };

        Self {
            name: nfo_title,
            original_title: &video.name,
            intro: &video.intro,
            season_number: video.season_number.unwrap_or(1), // 使用video的season_number，默认为1
            bvid: &video.bvid,
            upper_id: video.upper_id,
            upper_name: &video.upper_name,
            aired: aired_time,
            premiered: aired_time,
            tags: video
                .tags
                .as_ref()
                .and_then(|tags| serde_json::from_value(tags.clone()).ok()),
            user_rating: None,          // B站没有评分系统
            mpaa: None,                 // 使用默认分级
            country: None,              // 使用默认值（中国）
            studio: None,               // 使用默认值（哔哩哔哩）
            status: Some("Continuing"), // 默认持续播出状态
            total_episodes: None,       // 从集数统计推断
            duration: None,             // 平均集时长，需要计算
            view_count: None,           // video模型中没有view_count字段
            like_count: None,           // video模型中没有like_count字段
            category: video.category,
            tagline,
            set: set_name,
            sorttitle,
            actors_info: video.actors.clone(),
            staff_info: video.staff_info.as_ref(),
            cover_url: &video.cover,
            fanart_url: None, // 普通视频没有单独的fanart URL
            upper_face_url: if !video.upper_face.is_empty() {
                Some(&video.upper_face)
            } else {
                None
            },
            season_id: None, // 普通视频没有season_id
            media_id: None,  // 普通视频没有media_id
        }
    }
}

impl<'a> Season<'a> {
    pub fn from_video_with_collection(
        video: &'a video::Model,
        collection_name: Option<&'a str>,
        collection_cover: Option<&'a str>,
        season_number: i32,
        season_total_episodes: Option<i32>,
    ) -> Self {
        let mut season = Season::from(video);
        let safe_season = season_number.max(1);

        if let Some(name) = collection_name {
            season.name = name;
            season.original_title = name;
            season.sorttitle = Some(format!("{} 第{:02}季", name, safe_season));
            season.set = Some(name.to_string());
        }

        if let Some(cover) = collection_cover {
            season.cover_url = cover;
            season.fanart_url = Some(cover);
        }

        season.season_number = safe_season;
        season.total_episodes = season_total_episodes.map(|v| v.max(1));
        season
    }

    /// 从API获取的SeasonInfo创建带有完整元数据的Season
    pub fn from_season_info(video: &'a video::Model, season_info: &'a crate::workflow::SeasonInfo) -> Self {
        // 使用动态配置而非静态CONFIG
        let config = crate::config::reload_config();

        // 优先使用API的发布时间，如果没有则使用配置的时间类型
        let aired_time = if let Some(ref publish_time) = season_info.publish_time {
            // 使用统一的时间解析函数
            {
                let fallback_time = match config.nfo_config.time_type {
                    crate::config::NFOTimeType::FavTime => video.favtime,
                    crate::config::NFOTimeType::PubTime => video.pubtime,
                };
                parse_time_string(publish_time).unwrap_or(fallback_time)
            }
        } else {
            // 没有API时间，使用配置的时间类型
            match config.nfo_config.time_type {
                crate::config::NFOTimeType::FavTime => video.favtime,
                crate::config::NFOTimeType::PubTime => video.pubtime,
            }
        };

        // 使用API提供的信息
        let nfo_title = &season_info.title;
        let evaluate = season_info.evaluate.as_deref().unwrap_or(&video.intro);

        // 制作地区处理（使用第一个地区或默认值）
        let country = season_info.areas.first().map(|s| s.as_str());

        // 播出状态
        let status = season_info.status.as_deref();

        // 类型标签
        let genres: Option<Vec<String>> = if !season_info.styles.is_empty() {
            Some(season_info.styles.clone())
        } else {
            // 备选：使用video中的tags
            video
                .tags
                .as_ref()
                .and_then(|tags| serde_json::from_value(tags.clone()).ok())
        };

        Self {
            name: nfo_title,
            original_title: season_info.alias.as_deref().unwrap_or(&season_info.title),
            intro: evaluate,
            season_number: video.season_number.unwrap_or(1), // 使用video的season_number
            bvid: &video.bvid,
            upper_id: video.upper_id,
            upper_name: &video.upper_name,
            aired: aired_time,
            premiered: aired_time,
            tags: genres,
            user_rating: season_info.rating,
            mpaa: None, // 可以从API的"分级"字段获取，但目前API中没有
            country,
            studio: None, // 可以从制作公司获取，但API中暂无此字段
            status,
            total_episodes: season_info.total_episodes,
            duration: None, // 单集平均时长，需要计算
            view_count: season_info.total_views,
            like_count: season_info.total_favorites,
            category: video.category,
            tagline: season_info.alias.as_deref().map(|s| s.to_string()),
            set: if NFO::is_bangumi_video(video.category) {
                NFO::extract_bangumi_title_from_full_name(&season_info.title)
            } else {
                Some(season_info.title.clone())
            }, // 系列名称（清理季度信息）
            sorttitle: Some(season_info.title.clone()),
            actors_info: season_info.actors.clone(),
            staff_info: video.staff_info.as_ref(), // 番剧一般使用actors_info，staff_info作为备选
            cover_url: season_info
                .cover
                .as_deref()
                .or(season_info.horizontal_cover_169.as_deref())
                .or(season_info.horizontal_cover_1610.as_deref())
                .unwrap_or(&video.cover),
            fanart_url: season_info.cover.as_deref().filter(|s| !s.is_empty()),
            upper_face_url: if !video.upper_face.is_empty() {
                Some(&video.upper_face)
            } else {
                None
            },
            // 使用season_id和media_id作为额外的uniqueid
            season_id: Some(season_info.season_id.clone()),
            media_id: season_info.media_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_nfo() {
        let video = video::Model {
            intro: "intro".to_string(),
            name: "name".to_string(),
            upper_id: 1,
            upper_name: "upper_name".to_string(),
            cover: "https://example.com/cover.jpg".to_string(),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2022, 2, 2).unwrap(),
                chrono::NaiveTime::from_hms_opt(2, 2, 2).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2033, 3, 3).unwrap(),
                chrono::NaiveTime::from_hms_opt(3, 3, 3).unwrap(),
            ),
            bvid: "BV1nWcSeeEkV".to_string(),
            tags: Some(serde_json::json!(["tag1", "tag2"])),
            ..Default::default()
        };

        let generated_movie = NFO::Movie((&video).into()).generate_nfo().await.unwrap();
        // 由于XML字段顺序可能有差异，我们检查关键字段是否存在
        assert!(generated_movie.contains("<title>name</title>"));
        assert!(generated_movie.contains("<originaltitle>name</originaltitle>"));
        assert!(generated_movie.contains(r#"<uniqueid type="bilibili" default="true">BV1nWcSeeEkV</uniqueid>"#));
        assert!(generated_movie.contains("<country>中国</country>"));
        assert!(generated_movie.contains("<studio>哔哩哔哩</studio>"));
        assert!(generated_movie.contains("<name>1</name>")); // upper_id=1
        assert!(generated_movie.contains("<role>upper_name</role>"));
        assert!(generated_movie.contains("<thumb>https://example.com/cover.jpg</thumb>"));

        let generated_tvshow = NFO::TVShow((&video).into()).generate_nfo().await.unwrap();
        // 检查TVShow的关键字段
        assert!(generated_tvshow.contains("<title>name</title>"));
        assert!(generated_tvshow.contains("<originaltitle>name</originaltitle>"));
        assert!(generated_tvshow.contains(r#"<uniqueid type="bilibili" default="true">BV1nWcSeeEkV</uniqueid>"#));
        assert!(generated_tvshow.contains("<status>Continuing</status>"));
        assert!(generated_tvshow.contains("<totalseasons>1</totalseasons>"));
        assert!(generated_tvshow.contains("<country>中国</country>"));
        assert!(generated_tvshow.contains("<studio>哔哩哔哩</studio>"));
        assert!(generated_tvshow.contains("<name>1</name>")); // upper_id=1
        assert!(generated_tvshow.contains("<role>upper_name</role>"));
        assert!(generated_tvshow.contains("<thumb>https://example.com/cover.jpg</thumb>"));

        assert_eq!(
            NFO::Upper((&video).into()).generate_nfo().await.unwrap(),
            r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<person>
    <plot/>
    <outline/>
    <lockdata>false</lockdata>
    <dateadded>2033-03-03 03:03:03</dateadded>
    <title>upper_name</title>
    <sorttitle>upper_name</sorttitle>
    <uniqueid type="bilibili_uid" default="true">1</uniqueid>
</person>"#,
        );

        let page = page::Model {
            name: "name".to_string(),
            pid: 3,
            ..Default::default()
        };

        let generated_episode = NFO::Episode((&page).into()).generate_nfo().await.unwrap();
        // 检查Episode的关键字段
        assert!(generated_episode.contains("<title>name</title>"));
        assert!(generated_episode.contains("<originaltitle>name</originaltitle>"));
        assert!(generated_episode.contains("<season>1</season>"));
        assert!(generated_episode.contains("<episode>3</episode>"));
        assert!(generated_episode.contains(r#"<uniqueid type="bilibili" default="true">3</uniqueid>"#));
    }

    #[tokio::test]
    async fn test_empty_upper_name() {
        // 测试空UP主名称的处理
        let video = video::Model {
            intro: "测试视频介绍".to_string(),
            name: "官方内容".to_string(),
            upper_id: 0,
            upper_name: "".to_string(), // 空的UP主名称
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1234567890".to_string(),
            tags: Some(serde_json::json!(["官方", "番剧"])),
            ..Default::default()
        };

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证没有生成空的演员信息
        assert!(!movie_nfo.contains("<name></name>"));
        assert!(!movie_nfo.contains("<actor>"));

        let tvshow_nfo = NFO::TVShow((&video).into()).generate_nfo().await.unwrap();

        // 验证没有生成空的演员信息
        assert!(!tvshow_nfo.contains("<name></name>"));
        assert!(!tvshow_nfo.contains("<actor>"));

        println!("空UP主名称的Movie NFO:");
        println!("{}", movie_nfo);
        println!("\n空UP主名称的TVShow NFO:");
        println!("{}", tvshow_nfo);
    }

    #[tokio::test]
    async fn test_bangumi_title_optimization() {
        // 测试番剧标题优化
        let video = video::Model {
            intro: "数百年前，欲望催生了几乎灭绝人类的玛娜生态。".to_string(),
            name: "《灵笼 第二季》第1话 末世桃源".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1, // 番剧分类
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 5, 23).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 5, 23).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1bSJez1Et8".to_string(),
            tags: Some(serde_json::json!(["动画", "科幻"])),
            ..Default::default()
        };

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证标题优化
        assert!(movie_nfo.contains("<title>灵笼 第二季</title>"));
        assert!(movie_nfo.contains("<originaltitle>《灵笼 第二季》第1话 末世桃源</originaltitle>"));

        // 验证没有空的演员信息
        assert!(!movie_nfo.contains("<actor>"));

        println!("优化后的番剧Movie NFO:");
        println!("{}", movie_nfo);
    }

    #[tokio::test]
    async fn test_bangumi_tvshow_nfo_series_title_without_season() {
        // tvshow.nfo（番剧根目录）不应写入“第几季”
        let video = video::Model {
            intro: "测试番剧".to_string(),
            name: "测试番剧".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1, // 番剧分类
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            bvid: "BV1bSJez1Et8".to_string(),
            ..Default::default()
        };

        let season_info = crate::workflow::SeasonInfo {
            title: "仙王的日常生活 第五季".to_string(),
            episodes: vec![],
            alias: None,
            evaluate: None,
            rating: None,
            rating_count: None,
            areas: vec![],
            actors: None,
            styles: vec![],
            total_episodes: None,
            status: None,
            cover: Some("https://example.com/season5.jpg".to_string()),
            series_cover: Some("https://example.com/season1.jpg".to_string()),
            new_ep_cover: None,
            horizontal_cover_1610: None,
            horizontal_cover_169: None,
            bkg_cover: None,
            media_id: None,
            season_id: "1".to_string(),
            publish_time: None,
            total_views: None,
            total_favorites: None,
            show_season_type: None,
        };

        let tvshow = TVShow::from_season_info(&video, &season_info);
        let tvshow_nfo = NFO::TVShow(tvshow).generate_nfo().await.unwrap();

        assert!(tvshow_nfo.contains("<title>仙王的日常生活</title>"));
        assert!(tvshow_nfo.contains("<originaltitle>仙王的日常生活</originaltitle>"));
        assert!(tvshow_nfo.contains("<sorttitle>仙王的日常生活</sorttitle>"));
        assert!(!tvshow_nfo.contains("第五季"));
    }

    #[tokio::test]
    async fn test_nfo_kodi_compatibility() {
        // 测试生成的NFO文件是否符合Kodi标准
        let video = video::Model {
            intro: "测试视频介绍".to_string(),
            name: "测试视频".to_string(),
            upper_id: 123456,
            upper_name: "测试UP主".to_string(),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1234567890".to_string(),
            tags: Some(serde_json::json!(["科技", "教程", "编程"])),
            ..Default::default()
        };

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证Kodi Movie必需字段
        assert!(movie_nfo.contains("<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>"));
        assert!(movie_nfo.contains("<movie>"));
        assert!(movie_nfo.contains("</movie>"));
        assert!(movie_nfo.contains("<title>"));
        assert!(movie_nfo.contains("<originaltitle>"));
        assert!(movie_nfo.contains("<plot>"));
        assert!(movie_nfo.contains("<uniqueid"));
        assert!(movie_nfo.contains("type=\"bilibili\""));
        assert!(movie_nfo.contains("<year>"));
        assert!(movie_nfo.contains("<premiered>"));
        assert!(movie_nfo.contains("<aired>"));
        assert!(movie_nfo.contains("<studio>"));
        assert!(movie_nfo.contains("<country>"));

        let tvshow_nfo = NFO::TVShow((&video).into()).generate_nfo().await.unwrap();

        // 验证Kodi TVShow必需字段
        assert!(tvshow_nfo.contains("<tvshow>"));
        assert!(tvshow_nfo.contains("</tvshow>"));
        assert!(tvshow_nfo.contains("<status>"));
        assert!(tvshow_nfo.contains("<totalseasons>"));

        println!("生成的Movie NFO:");
        println!("{}", movie_nfo);
        println!("\n生成的TVShow NFO:");
        println!("{}", tvshow_nfo);
    }

    #[tokio::test]
    async fn test_empty_upper_strategy_configurations() {
        use crate::config::{EmptyUpperStrategy, NFOConfig};

        // 测试空UP主名称的各种处理策略
        let video = video::Model {
            intro: "测试视频介绍".to_string(),
            name: "官方内容".to_string(),
            upper_id: 0,
            upper_name: "".to_string(), // 空的UP主名称
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1234567890".to_string(),
            tags: Some(serde_json::json!(["官方", "番剧"])),
            ..Default::default()
        };

        // 测试Skip策略（默认）
        let config = NFOConfig {
            empty_upper_strategy: EmptyUpperStrategy::Skip,
            ..Default::default()
        };

        // 创建一个自定义的Movie结构并手动生成NFO
        let movie = Movie::from(&video);
        let actor_info = NFO::get_actor_info(movie.upper_id, movie.upper_name, &config);
        assert_eq!(actor_info, None);

        // 测试Placeholder策略
        let config = NFOConfig {
            empty_upper_strategy: EmptyUpperStrategy::Placeholder,
            empty_upper_placeholder: "官方内容".to_string(),
            ..Default::default()
        };

        let actor_info = NFO::get_actor_info(movie.upper_id, movie.upper_name, &config);
        assert_eq!(actor_info, Some(("官方内容".to_string(), "UP主".to_string())));

        // 测试Default策略
        let config = NFOConfig {
            empty_upper_strategy: EmptyUpperStrategy::Default,
            empty_upper_default_name: "哔哩哔哩".to_string(),
            ..Default::default()
        };

        let actor_info = NFO::get_actor_info(movie.upper_id, movie.upper_name, &config);
        assert_eq!(actor_info, Some(("哔哩哔哩".to_string(), "UP主".to_string())));

        // 测试非空UP主名称（昵称作为name，role固定为UP主）
        let actor_info = NFO::get_actor_info(123456, "测试UP主", &config);
        assert_eq!(actor_info, Some(("测试UP主".to_string(), "UP主".to_string())));

        // 测试无效UID（0或负数）时同样使用昵称
        let actor_info = NFO::get_actor_info(0, "测试UP主", &config);
        assert_eq!(actor_info, Some(("测试UP主".to_string(), "UP主".to_string())));

        println!("空UP主处理策略测试通过");
    }

    #[tokio::test]
    async fn test_enhanced_nfo_features() {
        // 测试增强的NFO功能，包括tagline、set、sorttitle等
        let video = video::Model {
            intro: "数百年前，欲望催生了几乎灭绝人类的玛娜生态。".to_string(),
            name: "《名侦探柯南 计时引爆摩天楼》柯南剧场版开山之作".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1,               // 番剧分类
            show_season_type: Some(2), // 影视剧场版
            share_copy: Some("《名侦探柯南 计时引爆摩天楼》柯南剧场版开山之作".to_string()),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 5, 23).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 5, 23).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1bSJez1Et8".to_string(),
            tags: Some(serde_json::json!(["推理", "智斗", "漫画改"])),
            ..Default::default()
        };

        let movie = Movie::from(&video);

        // 验证新字段被正确设置
        assert_eq!(movie.tagline, Some("柯南剧场版开山之作".to_string()));
        assert_eq!(movie.set, Some("名侦探柯南 计时引爆摩天楼".to_string()));
        assert!(movie.sorttitle.is_some());

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证NFO包含新字段
        assert!(movie_nfo.contains("<tagline>柯南剧场版开山之作</tagline>"));
        assert!(movie_nfo.contains("<sorttitle>名侦探柯南 计时引爆摩天楼</sorttitle>"));
        assert!(movie_nfo.contains("<set>"));
        assert!(movie_nfo.contains("<name>名侦探柯南 计时引爆摩天楼</name>"));

        // 验证标题提取
        assert!(movie_nfo.contains("<title>名侦探柯南 计时引爆摩天楼</title>"));
        assert!(movie_nfo.contains("<originaltitle>《名侦探柯南 计时引爆摩天楼》柯南剧场版开山之作</originaltitle>"));

        println!("增强NFO功能测试通过");
    }

    #[tokio::test]
    async fn test_subtitle_extraction() {
        // 测试副标题提取功能
        let test_cases = vec![
            (
                "《名侦探柯南 计时引爆摩天楼》柯南剧场版开山之作",
                Some("柯南剧场版开山之作"),
            ),
            ("《灵笼 第二季》第1话 末世桃源", None), // 包含集数信息，应该被过滤
            ("《进击的巨人》最终季", Some("最终季")),
            ("《名侦探柯南 水平线上的阴谋》日配 ", None), // 语言标签，应该被过滤
            ("《某某番剧》中配", None),                   // 语言标签，应该被过滤
            ("普通视频标题", None),
        ];

        for (input, expected) in test_cases {
            let result = NFO::extract_subtitle_from_share_copy(input);
            assert_eq!(result.as_deref(), expected, "Failed for input: {}", input);
        }

        println!("副标题提取测试通过");
    }

    #[tokio::test]
    async fn test_language_tag_filtering() {
        // 测试语言标签过滤功能
        let video = video::Model {
            intro: "故事以15年前的北大西洋上一件海难作序幕...".to_string(),
            name: "《名侦探柯南 水平线上的阴谋》日配 ".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1,               // 番剧分类
            show_season_type: Some(2), // 影视剧场版
            share_copy: Some("《名侦探柯南 水平线上的阴谋》日配 ".to_string()),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2020, 5, 22).unwrap(),
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2020, 5, 22).unwrap(),
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            bvid: "BV1Hz411q7vB".to_string(),
            tags: Some(serde_json::json!(["推理", "悬疑"])),
            ..Default::default()
        };

        let movie = Movie::from(&video);

        // 验证语言标签被过滤掉了
        assert_eq!(movie.tagline, None); // "日配"应该被过滤掉

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证NFO不包含语言标签作为tagline
        assert!(!movie_nfo.contains("<tagline>日配</tagline>"));
        assert!(!movie_nfo.contains("<tagline>中配</tagline>"));

        // 验证包含番剧默认类型标签
        assert!(movie_nfo.contains("<genre>动画</genre>"));
        assert!(movie_nfo.contains("<genre>剧场版</genre>"));

        // 验证标题提取正确
        assert!(movie_nfo.contains("<title>名侦探柯南 水平线上的阴谋</title>"));

        println!("语言标签过滤测试通过");
    }

    #[tokio::test]
    async fn test_actor_info_parsing() {
        // 测试演员信息解析功能
        let actors_str =
            "江户川柯南：高山南\n毛利兰：山崎和佳奈\n毛利小五郎：神谷明\n工藤新一：山口胜平\n目暮警部：茶风林";

        let actors = NFO::parse_actors_string(actors_str);

        assert_eq!(actors.len(), 5);
        assert_eq!(actors[0], ("江户川柯南".to_string(), "高山南".to_string()));
        assert_eq!(actors[1], ("毛利兰".to_string(), "山崎和佳奈".to_string()));
        assert_eq!(actors[2], ("毛利小五郎".to_string(), "神谷明".to_string()));
        assert_eq!(actors[3], ("工藤新一".to_string(), "山口胜平".to_string()));
        assert_eq!(actors[4], ("目暮警部".to_string(), "茶风林".to_string()));

        // 测试半角冒号格式
        let actors_en = NFO::parse_actors_string("Character1:Actor1\nCharacter2:Actor2");
        assert_eq!(actors_en.len(), 2);
        assert_eq!(actors_en[0], ("Character1".to_string(), "Actor1".to_string()));

        // 测试单独演员名（无角色分隔符）
        let actors_simple = NFO::parse_actors_string("演员名1\n演员名2");
        assert_eq!(actors_simple.len(), 2);
        assert_eq!(actors_simple[0], ("演员".to_string(), "演员名1".to_string()));

        println!("演员信息解析测试通过");
    }

    #[tokio::test]
    async fn test_nfo_with_real_actors() {
        // 测试使用真实演员信息生成NFO
        let video = video::Model {
            intro: "故事以15年前的北大西洋上一件海难作序幕...".to_string(),
            name: "《名侦探柯南 计时引爆摩天楼》".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1,               // 番剧分类
            show_season_type: Some(2), // 影视剧场版
            share_copy: Some("《名侦探柯南 计时引爆摩天楼》柯南剧场版开山之作".to_string()),
            actors: Some("江户川柯南：高山南\n毛利兰：山崎和佳奈\n毛利小五郎：神谷明".to_string()),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2020, 5, 22).unwrap(),
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2020, 5, 22).unwrap(),
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            ),
            bvid: "BV1Hz411q7vB".to_string(),
            tags: Some(serde_json::json!(["推理", "悬疑"])),
            ..Default::default()
        };

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证包含真实演员信息
        assert!(movie_nfo.contains("<actor>"));
        assert!(movie_nfo.contains("<name>高山南</name>"));
        assert!(movie_nfo.contains("<role>江户川柯南</role>"));
        assert!(movie_nfo.contains("<name>山崎和佳奈</name>"));
        assert!(movie_nfo.contains("<role>毛利兰</role>"));
        assert!(movie_nfo.contains("<order>1</order>"));
        assert!(movie_nfo.contains("<order>2</order>"));

        // 验证不包含UP主作为演员（因为有真实演员信息）
        assert!(!movie_nfo.contains("<role>创作者</role>"));

        println!("真实演员信息NFO生成测试通过");
    }

    #[tokio::test]
    async fn test_runtime_calculation() {
        // 测试视频时长计算功能
        let video = video::Model {
            intro: "测试时长计算".to_string(),
            name: "测试视频".to_string(),
            upper_id: 123456,
            upper_name: "测试UP主".to_string(),
            cover: "https://example.com/cover.jpg".to_string(),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1234567890".to_string(),
            tags: Some(serde_json::json!(["测试", "时长"])),
            ..Default::default()
        };

        // 创建测试页面数据（3分钟 + 5分钟 = 8分钟总时长）
        let pages = vec![
            page::Model {
                id: 1,
                video_id: 1,
                cid: 123,
                pid: 1,
                name: "第一页".to_string(),
                duration: 180, // 3分钟 = 180秒
                ..Default::default()
            },
            page::Model {
                id: 2,
                video_id: 1,
                cid: 124,
                pid: 2,
                name: "第二页".to_string(),
                duration: 300, // 5分钟 = 300秒
                ..Default::default()
            },
        ];

        // 测试带时长的Movie NFO生成
        let movie_with_duration = Movie::from_video_with_pages(&video, &pages);
        assert_eq!(movie_with_duration.duration, Some(8)); // 8分钟

        let movie_nfo = NFO::Movie(movie_with_duration).generate_nfo().await.unwrap();
        assert!(movie_nfo.contains("<runtime>8</runtime>"));
        assert!(movie_nfo.contains("<thumb>https://example.com/cover.jpg</thumb>"));

        // 测试带时长的TVShow NFO生成
        let tvshow_with_duration = TVShow::from_video_with_pages(&video, &pages);
        assert_eq!(tvshow_with_duration.duration, Some(8)); // 8分钟时长
        assert_eq!(tvshow_with_duration.total_episodes, None); // 总集数设为None避免显示错误信息

        let tvshow_nfo = NFO::TVShow(tvshow_with_duration).generate_nfo().await.unwrap();
        assert!(tvshow_nfo.contains("<runtime>8</runtime>"));
        assert!(tvshow_nfo.contains("<thumb>https://example.com/cover.jpg</thumb>"));
        assert!(!tvshow_nfo.contains("<totalepisodes>")); // 不应包含totalepisodes字段

        // 测试空页面数据的情况
        let empty_pages: Vec<page::Model> = vec![];
        let movie_no_duration = Movie::from_video_with_pages(&video, &empty_pages);
        assert_eq!(movie_no_duration.duration, None);

        println!("时长计算NFO生成测试通过");
    }

    #[tokio::test]
    async fn test_season_nfo_generation() {
        // 测试Season NFO生成功能
        let video = video::Model {
            intro: "数百年前，欲望催生了几乎灭绝人类的玛娜生态。".to_string(),
            name: "《灵笼 第二季》".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1,            // 番剧分类
            season_number: Some(2), // 第二季
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 5, 23).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 5, 23).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1bSJez1Et8".to_string(),
            tags: Some(serde_json::json!(["动画", "科幻"])),
            ..Default::default()
        };

        let season_nfo = NFO::Season((&video).into()).generate_nfo().await.unwrap();

        // 验证Season NFO的关键字段
        assert!(season_nfo.contains("<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>"));
        assert!(season_nfo.contains("<season>"));
        assert!(season_nfo.contains("</season>"));
        assert!(season_nfo.contains("<title>第二季</title>"));
        assert!(season_nfo.contains("<originaltitle>《灵笼 第二季》</originaltitle>"));
        assert!(season_nfo.contains("<seasonnumber>2</seasonnumber>"));
        assert!(season_nfo.contains(r#"<uniqueid type="bilibili" default="true">BV1bSJez1Et8</uniqueid>"#));
        assert!(season_nfo.contains("<country>中国</country>"));
        assert!(season_nfo.contains("<studio>哔哩哔哩</studio>"));
        assert!(season_nfo.contains("<status>Continuing</status>"));
        assert!(season_nfo.contains("<genre>动画</genre>"));
        assert!(season_nfo.contains("<genre>科幻</genre>"));

        // 验证没有空的演员信息
        assert!(!season_nfo.contains("<actor>"));

        println!("Season NFO生成测试通过");
        println!("生成的Season NFO:");
        println!("{}", season_nfo);
    }

    #[tokio::test]
    async fn test_dynamic_total_seasons_calculation() {
        // 测试阶段3修复：动态TotalSeasons计算功能

        // 测试不同季度标题的总季数计算
        let test_cases = vec![
            ("灵笼 第一季", 1),
            ("灵笼第二季", 2),
            ("进击的巨人第三季", 3),
            ("某科学的超电磁炮第2季", 2),
            ("鬼灭之刃第四季", 4),
            ("普通番剧标题", 1), // 无季度信息，默认为1
        ];

        for (input, expected) in test_cases {
            let result = NFO::calculate_total_seasons_from_title(input);
            assert_eq!(result, expected, "Failed for input: {}", input);
        }

        // 测试在TVShow NFO生成中的应用
        let video = video::Model {
            intro: "测试番剧".to_string(),
            name: "灵笼第二季".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1, // 番剧分类
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 7, 11).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 7, 11).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1TestSeason".to_string(),
            tags: Some(serde_json::json!(["科幻", "动画"])),
            ..Default::default()
        };

        let tvshow_nfo = NFO::TVShow((&video).into()).generate_nfo().await.unwrap();

        // 验证TVShow NFO中包含正确的总季数
        assert!(
            tvshow_nfo.contains("<totalseasons>2</totalseasons>"),
            "TVShow NFO应该包含正确的总季数2"
        );

        println!("动态TotalSeasons计算功能测试通过");
    }

    #[tokio::test]
    async fn test_season_title_fix() {
        // 测试Season NFO标题修复：Season的title应该显示完整季度名称
        let video = video::Model {
            intro: "数百年前，欲望催生了几乎灭绝人类的玛娜生态。".to_string(),
            name: "灵笼第二季".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1,            // 番剧分类
            season_number: Some(2), // 第二季
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 7, 11).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 7, 11).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1SeasonTitleFix".to_string(),
            tags: Some(serde_json::json!(["动画", "科幻"])),
            ..Default::default()
        };

        let season_nfo = NFO::Season((&video).into()).generate_nfo().await.unwrap();

        // 验证Season NFO的title显示纯季度名称（符合Emby标准）
        assert!(
            season_nfo.contains("<title>第二季</title>"),
            "Season NFO的title应该显示纯季度名称"
        );

        // 验证Season NFO的originaltitle
        assert!(
            season_nfo.contains("<originaltitle>灵笼第二季</originaltitle>"),
            "Season NFO的originaltitle应该正确"
        );

        // 验证Season NFO的seasonnumber
        assert!(
            season_nfo.contains("<seasonnumber>2</seasonnumber>"),
            "Season NFO应该包含正确的季度编号"
        );

        // 验证Season NFO的set使用系列名称（不含季度信息）
        assert!(season_nfo.contains("<set>"), "Season NFO应该包含set信息");
        assert!(
            season_nfo.contains("<name>灵笼</name>"),
            "Season NFO的set应该使用清理后的系列名称"
        );

        println!("Season标题修复测试通过");
    }

    #[tokio::test]
    async fn test_emby_standard_season_nfo() {
        // 测试符合Emby标准的Season NFO生成
        let video = video::Model {
            intro: "数百年前，欲望催生了几乎灭绝人类的玛娜生态。".to_string(),
            name: "灵笼第二季".to_string(),
            upper_id: 0,
            upper_name: "".to_string(),
            category: 1,            // 番剧分类
            season_number: Some(2), // 第二季
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 7, 11).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2025, 7, 11).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1EmbyStandard".to_string(),
            tags: Some(serde_json::json!(["动画", "科幻"])),
            ..Default::default()
        };

        let season_nfo = NFO::Season((&video).into()).generate_nfo().await.unwrap();

        // 验证Emby标准的Season NFO结构
        assert!(
            season_nfo.contains("<title>第二季</title>"),
            "Season NFO的title应该显示纯季度标题（第二季）"
        );

        assert!(
            season_nfo.contains("<originaltitle>灵笼第二季</originaltitle>"),
            "Season NFO的originaltitle应该保留完整名称"
        );

        assert!(
            season_nfo.contains("<seasonnumber>2</seasonnumber>"),
            "Season NFO应该包含正确的季度编号"
        );

        assert!(season_nfo.contains("<set>"), "Season NFO应该包含set信息");
        assert!(
            season_nfo.contains("<name>灵笼</name>"),
            "Season NFO的set应该使用清理后的系列名称"
        );

        // 验证Season专属的plot前缀
        assert!(
            season_nfo.contains("【第二季】"),
            "Season NFO的plot应该包含季度特定的前缀"
        );

        println!("Emby标准Season NFO测试通过");
        println!("生成的Season NFO:");
        println!("{}", season_nfo);
    }

    #[tokio::test]
    async fn test_nfo_actor_info_with_uid_and_role() {
        // 测试NFO生成中使用UID作为name，UP主名称作为role
        let video = video::Model {
            intro: "测试视频介绍".to_string(),
            name: "测试视频".to_string(),
            upper_id: 123456789, // 有效的UID
            upper_name: "知名UP主".to_string(),
            cover: "https://example.com/cover.jpg".to_string(),
            favtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            pubtime: chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2023, 6, 15).unwrap(),
                chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap(),
            ),
            bvid: "BV1TestUID123".to_string(),
            tags: Some(serde_json::json!(["科技", "教程"])),
            ..Default::default()
        };

        let movie_nfo = NFO::Movie((&video).into()).generate_nfo().await.unwrap();

        // 验证使用UID作为name，UP主名称作为role
        assert!(movie_nfo.contains("<actor>"));
        assert!(movie_nfo.contains("<name>123456789</name>")); // UID作为name
        assert!(movie_nfo.contains("<role>知名UP主</role>")); // UP主名称作为role
        assert!(movie_nfo.contains("<order>1</order>"));

        // 测试UID无效的情况
        let video_no_uid = video::Model {
            upper_id: 0, // 无效UID
            upper_name: "另一个UP主".to_string(),
            ..video
        };

        let movie_nfo_no_uid = NFO::Movie((&video_no_uid).into()).generate_nfo().await.unwrap();

        // 验证无效UID时，UP主名称同时作为name和role
        assert!(movie_nfo_no_uid.contains("<name>另一个UP主</name>"));
        assert!(movie_nfo_no_uid.contains("<role>另一个UP主</role>"));

        println!("NFO演员信息（UID和角色）测试通过");
    }
}
