use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use reqwest::{Client, Url};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use std::{env, fs, io::Read, path::Path, time::{Duration, SystemTime, UNIX_EPOCH}};

type HmacSha256 = Hmac<Sha256>;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let action = env::var("MEDIARY_PLUGIN_ACTION").unwrap_or_default();
    let settings = read_settings();
    let payload = read_action_input()?;
    let client = DockerCopilot::new(&settings)?;

    let result = match action.as_str() {
        "dashboard" => dashboard(&client, &settings, &payload).await?,
        "check_updates" => check_updates(&client, &settings).await?,
        "auto_update" => auto_update(&client, &settings).await?,
        "backup" => backup(&client).await?,
        _ => return Err(format!("不支持的 DC 助手动作: {action}")),
    };
    save_last_run(&action, &result);
    println!("{result}");
    Ok(())
}

fn read_settings() -> Map<String, Value> {
    env::var("MEDIARY_PLUGIN_SETTINGS_JSON")
        .ok()
        .and_then(|raw| serde_json::from_str::<Map<String, Value>>(&raw).ok())
        .unwrap_or_default()
}

fn read_action_input() -> Result<Value, String> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).map_err(|e| format!("读取动作参数失败: {e}"))?;
    if raw.trim().is_empty() { return Ok(Value::Null); }
    serde_json::from_str(&raw).map_err(|e| format!("动作参数不是有效 JSON: {e}"))
}

struct DockerCopilot { client: Client, host: Url, secret_key: String }

impl DockerCopilot {
    fn new(settings: &Map<String, Value>) -> Result<Self, String> {
        let host = setting(settings, "host").trim().trim_end_matches('/').to_owned();
        if host.is_empty() { return Err("请先配置 Docker Copilot 地址".into()); }
        let host = Url::parse(&(host + "/")).map_err(|_| "Docker Copilot 地址不是有效 URL".to_string())?;
        if !matches!(host.scheme(), "http" | "https") { return Err("Docker Copilot 地址必须使用 http 或 https".into()); }
        let secret_key = setting(settings, "secret_key").trim().to_owned();
        if secret_key.is_empty() { return Err("请先配置 Docker Copilot Secret Key".into()); }
        let client = Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        Ok(Self { client, host, secret_key })
    }

    fn endpoint(&self, path: &str) -> Result<Url, String> {
        self.host.join(path.trim_start_matches('/')).map_err(|_| "无法构造 Docker Copilot 请求地址".into())
    }

    fn bearer(&self) -> Result<String, String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "系统时间早于 Unix 纪元".to_string())?.as_secs();
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let claims = URL_SAFE_NO_PAD.encode(json!({"iat": now, "exp": now + 28 * 24 * 60 * 60}).to_string());
        let signed = format!("{header}.{claims}");
        let mut mac = HmacSha256::new_from_slice(self.secret_key.as_bytes()).map_err(|_| "无法创建 JWT 签名".to_string())?;
        mac.update(signed.as_bytes());
        Ok(format!("Bearer {signed}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())))
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let response = self.client.get(self.endpoint(path)?).header("Authorization", self.bearer()?).send().await.map_err(network_error)?;
        decode(response).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        let response = self.client.post(self.endpoint(path)?).header("Authorization", self.bearer()?).json(&body).send().await.map_err(network_error)?;
        decode(response).await
    }

    async fn delete(&self, path: &str) -> Result<Value, String> {
        let response = self.client.delete(self.endpoint(path)?).header("Authorization", self.bearer()?).send().await.map_err(network_error)?;
        decode(response).await
    }

    async fn containers(&self) -> Result<Vec<Value>, String> {
        let response = self.get("api/containers").await?;
        response_data_array(&response, 0, "读取容器列表")
    }

    async fn images(&self) -> Result<Vec<Value>, String> {
        let response = self.get("api/images").await?;
        response_data_array(&response, 200, "读取镜像列表")
    }
}

async fn decode(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let body = response.text().await.map_err(|e| format!("读取 Docker Copilot 响应失败: {e}"))?;
    if !status.is_success() { return Err(format!("Docker Copilot 返回 HTTP {}", status.as_u16())); }
    serde_json::from_str(&body).map_err(|_| "Docker Copilot 返回了无效 JSON".into())
}

fn network_error(error: reqwest::Error) -> String {
    if error.is_timeout() { "请求 Docker Copilot 超时".into() } else { "无法连接 Docker Copilot".into() }
}

fn response_data_array(response: &Value, expected_code: i64, operation: &str) -> Result<Vec<Value>, String> {
    if response.get("code").and_then(Value::as_i64) != Some(expected_code) {
        return Err(format!("{operation}失败: {}", message(response)));
    }
    Ok(response.get("data").and_then(Value::as_array).cloned().unwrap_or_default())
}

async fn check_updates(dc: &DockerCopilot, settings: &Map<String, Value>) -> Result<Value, String> {
    let selected = load_selection(settings).update_containers;
    let containers = dc.containers().await?;
    let updates = containers.iter().filter(|item| has_update(item) && (selected.is_empty() || selected.contains(&name(item)))).map(container_summary).collect::<Vec<_>>();
    let notice = if updates.is_empty() { "没有发现可更新的容器。".to_string() } else { format!("发现 {} 个可更新容器：{}。", updates.len(), updates.iter().filter_map(|v| v.get("name").and_then(Value::as_str)).collect::<Vec<_>>().join("、")) };
    Ok(json!({"notice": notice, "report": {"checked": containers.len(), "updates": updates}}))
}

async fn dashboard(
    dc: &DockerCopilot,
    settings: &Map<String, Value>,
    payload: &Value,
) -> Result<Value, String> {
    let containers = dc.containers().await?;
    let available = containers.iter().map(name).filter(|name| !name.is_empty()).collect::<std::collections::HashSet<_>>();
    let mut selection = load_selection(settings);
    let saving = payload.get("update_containers").is_some() || payload.get("auto_update_containers").is_some();
    if saving {
        selection.update_containers = payload_names(payload, "update_containers", &available);
        selection.auto_update_containers = payload_names(payload, "auto_update_containers", &available);
        save_selection(&selection)?;
    }
    let options = containers.iter().filter_map(|container| {
        let name = name(container);
        (!name.is_empty()).then(|| json!({"label": name.clone(), "value": name}))
    }).collect::<Vec<_>>();
    let items = containers.iter().filter_map(|container| {
        let name = name(container);
        (!name.is_empty()).then(|| json!({
            "key": name.clone(),
            "title": name,
            "subtitle": text(container, "usingImage"),
            "badges": [{
                "label": if has_update(container) { "可更新" } else { "已是最新" },
                "tone": if has_update(container) { "warning" } else { "success" }
            }],
            "metadata": [
                {"label": "状态", "value": text(container, "status")},
                {"label": "运行时长", "value": text(container, "runningTime")}
            ]
        }))
    }).collect::<Vec<_>>();
    Ok(json!({
        "notice": if saving { "容器选择已保存。" } else { "已从 Docker Copilot 加载容器。" },
        "items": items,
        "form_options": {
            "update_containers": options.clone(),
            "auto_update_containers": options
        },
        "form_values": {
            "update_containers": selection.update_containers.into_iter().collect::<Vec<_>>(),
            "auto_update_containers": selection.auto_update_containers.into_iter().collect::<Vec<_>>()
        }
    }))
}

async fn auto_update(dc: &DockerCopilot, settings: &Map<String, Value>) -> Result<Value, String> {
    let mut cleaned = Vec::new();
    if setting_bool(settings, "clean_unused_images", false) {
        for image in dc.images().await? {
            if !is_in_use(&image) && is_untagged(&image) {
                let id = text(&image, "id");
                if !id.is_empty() {
                    let response = dc.delete(&format!("api/image/{id}?force=false")).await?;
                    if response.get("code").and_then(Value::as_i64) == Some(200) { cleaned.push(id); }
                }
            }
        }
    }
    let selected = load_selection(settings).auto_update_containers;
    if selected.is_empty() { return Ok(json!({"notice": "未设置自动更新容器，未执行更新。", "report": {"cleaned_images": cleaned, "updated": []}})); }
    let mut updated = Vec::new();
    let mut skipped = Vec::new();
    for container in dc.containers().await? {
        let name = name(&container);
        if !has_update(&container) || !selected.contains(&name) { continue; }
        let image = text(&container, "usingImage");
        if image.is_empty() || image.starts_with("sha256:") {
            skipped.push(json!({"name": name, "reason": "当前镜像没有可用标签，无法通过 Docker Copilot 自动更新"}));
            continue;
        }
        let id = text(&container, "id");
        if id.is_empty() { skipped.push(json!({"name": name, "reason": "容器缺少 ID"})); continue; }
        let response = dc.post(&format!("api/container/{id}/update"), json!({"containerName": name, "imageNameAndTag": image})).await?;
        if response.get("code").and_then(Value::as_i64) == Some(200) && message(&response) == "success" {
            let mut item = json!({"name": name, "status": "任务已创建"});
            if setting_bool(settings, "track_progress", true) {
                if let Some(task_id) = response.pointer("/data/taskID").and_then(Value::as_str) { item["progress"] = Value::String(track_progress(dc, task_id, settings).await?); }
            }
            updated.push(item);
        } else { skipped.push(json!({"name": name, "reason": message(&response)})); }
    }
    let notice = format!("自动更新完成：创建 {} 个任务，跳过 {} 个容器，清理 {} 个镜像。", updated.len(), skipped.len(), cleaned.len());
    Ok(json!({"notice": notice, "report": {"cleaned_images": cleaned, "updated": updated, "skipped": skipped}}))
}

async fn track_progress(dc: &DockerCopilot, task_id: &str, settings: &Map<String, Value>) -> Result<String, String> {
    let attempts = setting_u64(settings, "progress_attempts", 6).clamp(1, 100);
    let interval = setting_u64(settings, "progress_interval_seconds", 10).clamp(1, 300);
    let mut latest = "等待更新进度".to_string();
    for attempt in 0..attempts {
        let response = dc.get(&format!("api/progress/{task_id}")).await?;
        if response.get("code").and_then(Value::as_i64) == Some(200) {
            latest = message(&response);
            if latest == "更新成功" { break; }
        }
        if attempt + 1 < attempts { tokio::time::sleep(Duration::from_secs(interval)).await; }
    }
    Ok(latest)
}

async fn backup(dc: &DockerCopilot) -> Result<Value, String> {
    let response = dc.get("api/container/backup").await?;
    if response.get("code").and_then(Value::as_i64) == Some(200) { Ok(json!({"notice": "容器配置备份成功。", "report": {"message": message(&response)}})) } else { Err(format!("容器配置备份失败: {}", message(&response))) }
}

fn save_last_run(action: &str, result: &Value) {
    let Ok(data_dir) = env::var("MEDIARY_PLUGIN_DATA_DIR") else { return; };
    let path = Path::new(&data_dir);
    if fs::create_dir_all(path).is_err() { return; }
    let record = json!({"action": action, "ok": true, "notice": result.get("notice").and_then(Value::as_str).unwrap_or(""), "completed_at": unix_time()});
    let temp = path.join("last-run.json.tmp");
    if fs::write(&temp, record.to_string()).is_ok() { let _ = fs::rename(temp, path.join("last-run.json")); }
}

struct ContainerSelection {
    update_containers: std::collections::HashSet<String>,
    auto_update_containers: std::collections::HashSet<String>,
}

fn load_selection(settings: &Map<String, Value>) -> ContainerSelection {
    let fallback = ContainerSelection {
        update_containers: names(setting(settings, "update_containers")),
        auto_update_containers: names(setting(settings, "auto_update_containers")),
    };
    let Ok(data_dir) = env::var("MEDIARY_PLUGIN_DATA_DIR") else { return fallback; };
    let Ok(raw) = fs::read_to_string(Path::new(&data_dir).join("container-selection.json")) else { return fallback; };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else { return fallback; };
    ContainerSelection {
        update_containers: value_names(&value, "update_containers"),
        auto_update_containers: value_names(&value, "auto_update_containers"),
    }
}

fn save_selection(selection: &ContainerSelection) -> Result<(), String> {
    let data_dir = env::var("MEDIARY_PLUGIN_DATA_DIR").map_err(|_| "Mediary 未提供插件数据目录，无法保存容器选择".to_string())?;
    let path = Path::new(&data_dir);
    fs::create_dir_all(path).map_err(|_| "无法创建插件数据目录".to_string())?;
    let record = json!({
        "update_containers": selection.update_containers.iter().cloned().collect::<Vec<_>>(),
        "auto_update_containers": selection.auto_update_containers.iter().cloned().collect::<Vec<_>>(),
    });
    let temp = path.join("container-selection.json.tmp");
    fs::write(&temp, record.to_string()).map_err(|_| "无法写入容器选择".to_string())?;
    fs::rename(temp, path.join("container-selection.json")).map_err(|_| "无法保存容器选择".to_string())
}

fn unix_time() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|value| value.as_secs()).unwrap_or(0) }
fn setting<'a>(settings: &'a Map<String, Value>, key: &str) -> &'a str { settings.get(key).and_then(Value::as_str).unwrap_or("") }
fn setting_bool(settings: &Map<String, Value>, key: &str, default: bool) -> bool { settings.get(key).and_then(Value::as_bool).unwrap_or(default) }
fn setting_u64(settings: &Map<String, Value>, key: &str, default: u64) -> u64 { settings.get(key).and_then(Value::as_u64).unwrap_or(default) }
fn text(value: &Value, key: &str) -> String { value.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string() }
fn name(value: &Value) -> String { text(value, "name") }
fn message(value: &Value) -> String { text(value, "msg") }
fn has_update(value: &Value) -> bool { value.get("haveUpdate").and_then(Value::as_bool).unwrap_or(false) }
fn is_in_use(value: &Value) -> bool { value.get("inUsed").and_then(Value::as_bool).unwrap_or(false) }
fn is_untagged(value: &Value) -> bool { match value.get("tag") { None | Some(Value::Null) => true, Some(Value::String(tag)) => tag.trim().is_empty() || tag == "<none>", Some(Value::Array(tags)) => tags.is_empty(), _ => false } }
fn names(raw: &str) -> std::collections::HashSet<String> { raw.split(',').map(str::trim).filter(|name| !name.is_empty()).map(ToOwned::to_owned).collect() }
fn value_names(value: &Value, key: &str) -> std::collections::HashSet<String> { value.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).map(ToOwned::to_owned).collect() }
fn payload_names(payload: &Value, key: &str, available: &std::collections::HashSet<String>) -> std::collections::HashSet<String> { value_names(payload, key).into_iter().filter(|name| available.contains(name)).collect() }
fn container_summary(value: &Value) -> Value { json!({"name": name(value), "image": text(value, "usingImage"), "status": text(value, "status"), "running_time": text(value, "runningTime"), "created_at": text(value, "createTime")}) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_untagged_images_without_deleting_tagged_ones() {
        assert!(is_untagged(&json!({"tag": "<none>"})));
        assert!(is_untagged(&json!({"tag": []})));
        assert!(!is_untagged(&json!({"tag": "linuxserver/emby:latest"})));
    }
    #[test]
    fn parses_container_lists_without_empty_names() {
        let parsed = names(" emby, ,qbittorrent ");
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains("emby"));
    }
}
