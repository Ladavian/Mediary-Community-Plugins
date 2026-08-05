use chrono::{Local, NaiveDate};
use quick_xml::{Reader, events::Event};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::{HashSet, VecDeque}, env, fs, io::Read, path::{Path, PathBuf}, time::Duration};

const MAX_RECORDS: usize = 2_000;
const USER_AGENT: &str = "Mediary-Douban-Coming/1.0";

#[derive(Clone)]
struct Context {
    api_url: String,
    token: String,
    data_dir: PathBuf,
    client: Client,
    douban_client: Client,
    settings: Map<String, Value>,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    #[serde(default)] runs: usize,
    #[serde(default)] subscribed: usize,
    #[serde(default)] notifications: usize,
    #[serde(default)] records: VecDeque<Record>,
    #[serde(default)] notified: HashSet<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    title: String,
    wish_count: i64,
    air_date: String,
    subscription: String,
    reminder: String,
    processed_at: String,
}

#[derive(Clone)]
struct FeedItem { title: String, link: String, description: String, wish_count: i64, year: Option<i32> }

#[derive(Deserialize)]
struct ResolvedMedia {
    tmdb_id: String,
    #[serde(default)] title: String,
    #[serde(default, rename = "type")] media_type: String,
    year: Option<i32>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    vote_average: Option<f64>,
    description: Option<String>,
    expected_episodes: Option<i32>,
    first_air_date: Option<String>,
    air_date: Option<String>,
    release_date: Option<String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await { eprintln!("{error}"); std::process::exit(1); }
}

async fn run() -> Result<(), String> {
    if env::var("MEDIARY_PLUGIN_ACTION").unwrap_or_default() != "refresh" { return Err("不支持的豆瓣将映动作".into()); }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).map_err(|e| format!("读取动作参数失败: {e}"))?;
    if !input.trim().is_empty() { serde_json::from_str::<Value>(&input).map_err(|e| format!("动作参数不是有效 JSON: {e}"))?; }
    let context = Context::from_env()?;
    let report = refresh(&context).await?;
    println!("{}", json!({"notice": format!("豆瓣将映刷新完成：获取 {}，新增订阅 {}，发送提醒 {}。", report.fetched, report.subscribed, report.notifications), "report": report}));
    Ok(())
}

#[derive(Serialize)]
struct Report { fetched: usize, considered: usize, subscribed: usize, notifications: usize, skipped: usize, failures: Vec<String> }

impl Context {
    fn from_env() -> Result<Self, String> {
        let api_url = required("MEDIARY_PLUGIN_API_URL")?.trim_end_matches('/').to_string();
        let token = required("MEDIARY_PLUGIN_TOKEN")?;
        let data_dir = PathBuf::from(required("MEDIARY_PLUGIN_DATA_DIR")?);
        fs::create_dir_all(&data_dir).map_err(|e| format!("创建数据目录失败: {e}"))?;
        let settings = env::var("MEDIARY_PLUGIN_SETTINGS_JSON").ok().and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default();
        let client = Client::builder().user_agent(USER_AGENT).timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
        let douban_client = Client::builder()
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(10))
            .build().map_err(|e| e.to_string())?;
        Ok(Self { api_url, token, data_dir, client, douban_client, settings })
    }
}

async fn refresh(context: &Context) -> Result<Report, String> {
    let history_path = context.data_dir.join("history.json");
    let mut history = if setting_bool(context, "clear", false) { History::default() } else { load_json(&history_path) };
    let items = fetch_feed(context).await?;
    let mut report = Report { fetched: items.len(), considered: 0, subscribed: 0, notifications: 0, skipped: 0, failures: Vec::new() };
    let mut existing = existing_subscriptions(context).await?;
    for item in items {
        if item.wish_count < setting_i64(context, "wish_count_threshold", 5000).max(0) { report.skipped += 1; continue; }
        report.considered += 1;
        let resolved = match resolve(context, &item).await { Ok(value) => value, Err(error) => { report.failures.push(format!("{}: {error}", item.title)); continue; } };
        if resolved.media_type.trim().to_ascii_lowercase() != "tv" { report.skipped += 1; continue; }
        if let (Some(feed_year), Some(resolved_year)) = (item.year, resolved.year) {
            if feed_year != resolved_year {
                report.failures.push(format!("{}: TMDB 年份不一致（RSS {feed_year}，TMDB {resolved_year}）", item.title));
                continue;
            }
        }
        let title = if resolved.title.trim().is_empty() { item.title.clone() } else { resolved.title.clone() };
        let air_date = if let Some(date) = media_air_date(&resolved).or_else(|| extract_date(&item.description)) {
            Some(date)
        } else {
            fetch_douban_air_date(context, &item.link).await.ok().flatten()
        }.map(|date| date.format("%Y-%m-%d").to_string());
        let key = subscription_key(&resolved.tmdb_id, 1);
        let days = air_date.as_deref().and_then(days_until);
        let mut subscription = "已存在".to_string();
        let mut created = false;
        if !existing.contains(&key) && days.is_some_and(|value| value >= 0 && value <= setting_i64(context, "advance_days", 7).max(0)) {
            if let Err(error) = create_subscription(context, &item, &resolved).await { report.failures.push(format!("{title}: 创建订阅失败: {error}")); continue; }
            existing.insert(key);
            history.subscribed += 1;
            report.subscribed += 1;
            subscription = "已创建".to_string();
            created = true;
        } else if !existing.contains(&subscription_key(&resolved.tmdb_id, 1)) { subscription = if days.is_none() { "日期未知".to_string() } else { "未到订阅窗口".to_string() }; }
        let mut reminder = "未提醒".to_string();
        let notice_key = format!("{}:{}", resolved.tmdb_id, air_date.clone().unwrap_or_default());
        if setting_bool(context, "notify_before_air", true) && !history.notified.contains(&notice_key) && air_date.as_deref().and_then(hours_until).is_some_and(|value| value >= 0.0 && value <= setting_i64(context, "notify_hours", 24).max(1) as f64) {
            let content = format!("名称：{title}\n开播日期：{}\n想看人数：{}\n订阅状态：{}\n豆瓣链接：{}", air_date.clone().unwrap_or_else(|| "-".into()), item.wish_count, if created || existing.contains(&subscription_key(&resolved.tmdb_id, 1)) { "已订阅" } else { "未订阅" }, item.link);
            send_notification(context, "豆瓣将映提醒", &content, resolved.poster_path.as_deref()).await?;
            history.notified.insert(notice_key);
            history.notifications += 1;
            report.notifications += 1;
            reminder = "已发送".to_string();
        }
        history.records.retain(|record| record.title != title || record.air_date != air_date.clone().unwrap_or_default());
        history.records.push_front(Record { title, wish_count: item.wish_count, air_date: air_date.unwrap_or_else(|| "未知".into()), subscription, reminder, processed_at: Local::now().to_rfc3339() });
    }
    history.runs += 1;
    while history.records.len() > MAX_RECORDS { history.records.pop_back(); }
    write_json(&history_path, &history)?;
    Ok(report)
}

async fn fetch_douban_air_date(context: &Context, link: &str) -> Result<Option<NaiveDate>, String> {
    if !link.starts_with("https://movie.douban.com/") { return Ok(None); }
    let url = link.replace("https://movie.douban.com/", "https://m.douban.com/movie/");
    let response = context.douban_client.get(url).header("Accept-Language", "zh-CN,zh;q=0.9").send().await.map_err(|e| format!("请求豆瓣详情页失败: {e}"))?;
    if !response.status().is_success() { return Err(format!("豆瓣详情页返回 HTTP {}", response.status())); }
    let html = response.text().await.map_err(|e| format!("读取豆瓣详情页失败: {e}"))?;
    Ok(extract_douban_date(&html))
}

fn extract_douban_date(html: &str) -> Option<NaiveDate> {
    let regex = Regex::new(r"(?:首播|上映日期|上映时间|开播)\s*[:：]?[\s\S]{0,60}?((?:19|20)\d{2})[-/年.](\d{1,2})[-/月.](\d{1,2})|((?:19|20)\d{2})[-/年.](\d{1,2})[-/月.](\d{1,2})\s*\([^)]{0,12}\)\s*上映").ok()?;
    let captures = regex.captures(html)?;
    if captures.get(1).is_some() {
        NaiveDate::from_ymd_opt(captures.get(1)?.as_str().parse().ok()?, captures.get(2)?.as_str().parse().ok()?, captures.get(3)?.as_str().parse().ok()?)
    } else {
        NaiveDate::from_ymd_opt(captures.get(4)?.as_str().parse().ok()?, captures.get(5)?.as_str().parse().ok()?, captures.get(6)?.as_str().parse().ok()?)
    }
}

async fn fetch_feed(context: &Context) -> Result<Vec<FeedItem>, String> {
    let base = setting_text(context, "rsshub", "https://rsshub.ddsrem.com").trim_end_matches('/').to_string();
    let sort = match setting_text(context, "sort_by", "hot") { "time" => "time", _ => "hot" };
    let count = setting_i64(context, "count", 10).clamp(1, 100);
    let body = context.client.get(format!("{base}/douban/tv/coming/{sort}/{count}")).send().await.map_err(|e| format!("请求 RSSHub 失败: {e}"))?.error_for_status().map_err(|e| format!("RSSHub 返回错误: {e}"))?.text().await.map_err(|e| format!("读取 RSSHub 响应失败: {e}"))?;
    parse_rss(&body)
}

fn parse_rss(xml: &str) -> Result<Vec<FeedItem>, String> {
    let mut reader = Reader::from_str(xml); reader.config_mut().trim_text(true);
    let mut buffer = Vec::new(); let mut field = String::new(); let mut current: Map<String, Value> = Map::new(); let mut output = Vec::new(); let mut in_item = false;
    loop { match reader.read_event_into(&mut buffer) {
        Ok(Event::Start(event)) => { let name = String::from_utf8_lossy(event.name().as_ref()).to_string(); if name == "item" { in_item = true; current.clear(); } else if in_item { field = name; } }
        Ok(Event::Text(event)) => if in_item && !field.is_empty() {
            let text = event.unescape().map_err(|e| e.to_string())?.into_owned();
            if let Some(value) = current.get_mut(&field) {
                let previous = value.as_str().unwrap_or_default();
                *value = Value::String(format!("{previous}{text}"));
            } else {
                current.insert(field.clone(), Value::String(text));
            }
        }
        Ok(Event::CData(event)) => if in_item && !field.is_empty() { let text = String::from_utf8_lossy(event.as_ref()).to_string(); current.insert(field.clone(), Value::String(text)); }
        Ok(Event::End(event)) => { let name = String::from_utf8_lossy(event.name().as_ref()).to_string(); if name == "item" { let title = current.get("title").and_then(Value::as_str).unwrap_or("").trim().to_string(); let link = current.get("link").and_then(Value::as_str).unwrap_or("").trim().to_string(); let description = current.get("description").and_then(Value::as_str).unwrap_or("").to_string(); let category = current.get("category").and_then(Value::as_str).unwrap_or(""); if !title.is_empty() || !link.is_empty() { output.push(FeedItem { year: extract_year(category).or_else(|| extract_year(&description)), wish_count: extract_wish_count(&description), title, link, description }); } in_item = false; field.clear(); } else if name == field { field.clear(); } }
        Ok(Event::Eof) => break,
        Err(error) => return Err(format!("解析 RSS 失败: {error}")),
        _ => {}
    } buffer.clear(); }
    Ok(output)
}

async fn resolve(context: &Context, item: &FeedItem) -> Result<ResolvedMedia, String> {
    let response = api_post(context, "/tmdb/resolve", json!({"title": item.title, "year": item.year, "media_type": "tv"})).await?;
    if response.get("status").and_then(Value::as_str) == Some("failed") { return Err(response.get("message").and_then(Value::as_str).unwrap_or("TMDB 未匹配").into()); }
    serde_json::from_value(response.get("data").cloned().unwrap_or(response)).map_err(|e| format!("TMDB 响应格式无效: {e}"))
}

async fn existing_subscriptions(context: &Context) -> Result<HashSet<String>, String> {
    let values = api_get(context, "/subscriptions").await?.as_array().cloned().ok_or_else(|| "订阅列表响应格式无效".to_string())?;
    Ok(values.iter().filter_map(|value| {
        let kind = value.get("media_type")?.as_str()?;
        let id = value_id(value.get("tmdb_id")?)?;
        kind.eq_ignore_ascii_case("tv").then(|| subscription_key(&id, value.get("season").and_then(Value::as_i64).unwrap_or(1)))
    }).collect())
}

async fn create_subscription(context: &Context, item: &FeedItem, media: &ResolvedMedia) -> Result<(), String> {
    api_post(context, "/subscriptions", json!({"tmdb_id": media.tmdb_id, "name": if media.title.trim().is_empty() { &item.title } else { &media.title }, "year": media.year.or(item.year), "season": 1, "media_type": "tv", "poster_path": media.poster_path.as_deref(), "backdrop_path": media.backdrop_path.as_deref(), "vote_average": media.vote_average, "description": media.description.as_deref(), "expected_episodes": media.expected_episodes})).await.map(|_| ())
}

async fn send_notification(context: &Context, title: &str, content: &str, image_url: Option<&str>) -> Result<(), String> { api_post(context, "/plugin/notifications", json!({"title": title, "content": content, "image_url": image_url})).await.map(|_| ()) }
async fn api_get(context: &Context, path: &str) -> Result<Value, String> { parse_response(context.client.get(format!("{}{}", context.api_url, path)).bearer_auth(&context.token).send().await.map_err(|e| format!("请求 Mediary 失败: {e}"))?).await }
async fn api_post(context: &Context, path: &str, body: Value) -> Result<Value, String> { parse_response(context.client.post(format!("{}{}", context.api_url, path)).bearer_auth(&context.token).json(&body).send().await.map_err(|e| format!("请求 Mediary 失败: {e}"))?).await }
async fn parse_response(response: reqwest::Response) -> Result<Value, String> { let status = response.status(); let body = response.text().await.map_err(|e| e.to_string())?; if !status.is_success() { return Err(format!("Mediary API {status}")); } serde_json::from_str(&body).map_err(|e| format!("解析 Mediary 响应失败: {e}")) }

fn media_air_date(media: &ResolvedMedia) -> Option<NaiveDate> { media.first_air_date.as_deref().or(media.air_date.as_deref()).or(media.release_date.as_deref()).and_then(extract_date) }
fn extract_date(value: &str) -> Option<NaiveDate> { let regex = Regex::new(r"(?P<year>19\d{2}|20\d{2})[-/年.](?P<month>\d{1,2})[-/月.](?P<day>\d{1,2})").ok()?; let captures = regex.captures(value)?; NaiveDate::from_ymd_opt(captures.name("year")?.as_str().parse().ok()?, captures.name("month")?.as_str().parse().ok()?, captures.name("day")?.as_str().parse().ok()?) }
fn extract_year(value: &str) -> Option<i32> { Regex::new(r"(?:19|20)\d{2}").ok()?.find(value)?.as_str().parse().ok() }
fn extract_wish_count(value: &str) -> i64 { let compact = value.replace([',', '，'], ""); Regex::new(r"(?:想看人数|想看)\s*[:：]?\s*(\d+)|(\d+)\s*人想看").ok().and_then(|regex| regex.captures(&compact)).and_then(|captures| captures.get(1).or_else(|| captures.get(2))).and_then(|matched| matched.as_str().parse().ok()).unwrap_or(0) }
fn days_until(value: &str) -> Option<i64> { NaiveDate::parse_from_str(value, "%Y-%m-%d").ok().map(|date| (date - Local::now().date_naive()).num_days()) }
fn hours_until(value: &str) -> Option<f64> { let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?; let air_time = date.and_hms_opt(0, 0, 0)?.and_local_timezone(Local).single()?; Some((air_time - Local::now()).num_seconds() as f64 / 3600.0) }
fn subscription_key(tmdb_id: &str, season: i64) -> String { format!("tv:{}:{}", tmdb_id.trim(), season) }
fn value_id(value: &Value) -> Option<String> { value.as_str().map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned).or_else(|| value.as_i64().map(|value| value.to_string())) }
fn required(key: &str) -> Result<String, String> { env::var(key).map_err(|_| format!("缺少环境变量 {key}")) }
fn setting_text<'a>(context: &'a Context, key: &str, default: &'a str) -> &'a str { context.settings.get(key).and_then(Value::as_str).unwrap_or(default) }
fn setting_i64(context: &Context, key: &str, default: i64) -> i64 { context.settings.get(key).and_then(Value::as_i64).and_then(|value| i64::try_from(value).ok()).unwrap_or(default) }
fn setting_bool(context: &Context, key: &str, default: bool) -> bool { context.settings.get(key).and_then(Value::as_bool).unwrap_or(default) }
fn load_json<T: for<'a> Deserialize<'a> + Default>(path: &Path) -> T { fs::read_to_string(path).ok().and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default() }
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> { let temp = path.with_extension("tmp"); fs::write(&temp, serde_json::to_vec(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?; fs::rename(temp, path).map_err(|e| e.to_string()) }

#[cfg(test)]
mod tests { use super::*; #[test] fn extracts_feed_values() { assert_eq!(extract_wish_count("已有 12,345 人想看"), 12345); assert_eq!(extract_date("首播 2026-08-02"), NaiveDate::from_ymd_opt(2026, 8, 2)); assert_eq!(extract_year("2027 中国大陆"), Some(2027)); } #[test] fn extracts_douban_page_date() { assert_eq!(extract_douban_date(r#"<span class="pl">首播:</span> 2026-08-20(中国大陆) <br/>"#), NaiveDate::from_ymd_opt(2026, 8, 20)); assert_eq!(extract_douban_date(r#"<span class="pl">上映日期:</span> 2026年9月1日 <br/>"#), NaiveDate::from_ymd_opt(2026, 9, 1)); assert_eq!(extract_douban_date(r#"中国大陆 / 剧情 / 古装 / 2026-08-09(中国大陆)上映 / 片长45分钟"#), NaiveDate::from_ymd_opt(2026, 8, 9)); assert_eq!(extract_douban_date("<p>没有日期</p>"), None); } #[test] fn parses_rss_items() { let feed = "<rss><channel><item><title>剧集</title><link>https://movie.douban.com/subject/1/</link><description>5000人想看</description><category>2027 / 中国大陆</category></item></channel></rss>"; let items = parse_rss(feed).unwrap(); assert_eq!(items.len(), 1); assert_eq!(items[0].wish_count, 5000); assert_eq!(items[0].year, Some(2027)); } }
