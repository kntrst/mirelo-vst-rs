use std::path::PathBuf;

fn app_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("MireloVst"))
}

/// `%APPDATA%\MireloVst\samples`
pub fn samples_dir() -> Option<PathBuf> {
    app_dir().map(|d| d.join("samples"))
}

fn config_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("config.json"))
}

pub fn load_api_key() -> String {
    let from_file = config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("api_key")?.as_str().map(str::to_owned))
        .filter(|k| !k.is_empty());
    from_file
        .or_else(|| std::env::var("MIRELO_API_KEY").ok())
        .unwrap_or_default()
}

pub fn save_api_key(key: &str) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| std::io::Error::other("no data dir"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::json!({ "api_key": key.trim() });
    std::fs::write(path, body.to_string())
}
