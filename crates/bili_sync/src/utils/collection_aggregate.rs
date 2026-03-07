use anyhow::Result;

use crate::bilibili::BiliClient;

pub fn collection_type_name_from_db(collection_type: i32) -> &'static str {
    if collection_type == 1 {
        "series"
    } else {
        "season"
    }
}

pub fn build_bili_client_from_config() -> BiliClient {
    let config = crate::config::reload_config();
    let credential = config.credential.load();
    let cookie = credential
        .as_ref()
        .map(|cred| {
            format!(
                "SESSDATA={};bili_jct={};buvid3={};DedeUserID={};ac_time_value={}",
                cred.sessdata, cred.bili_jct, cred.buvid3, cred.dedeuserid, cred.ac_time_value
            )
        })
        .unwrap_or_default();
    BiliClient::new(cookie)
}

pub async fn fetch_absolute_collection_season_number(
    up_mid: i64,
    collection_sid: i64,
    collection_type: i32,
) -> Result<Option<i32>> {
    let bili_client = build_bili_client_from_config();
    let response = bili_client.get_user_collections(up_mid, 1, 50).await?;
    let target_sid = collection_sid.to_string();
    let target_type = collection_type_name_from_db(collection_type);

    Ok(response
        .collections
        .iter()
        .position(|item| item.sid == target_sid && item.collection_type == target_type)
        .map(|index| index as i32 + 1))
}
