use std::error::Error;
use serde::Deserialize;
use tokio::{fs::File, io::AsyncReadExt};
use serde_json::Value;

#[derive(Deserialize)]
struct Toggl {
    api_token: String,
}

#[derive(Deserialize)]
struct Config {
    toggl: Toggl,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut f = File::open("conf/config.toml").await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    let config = toml::from_slice::<Config>(&buf)?;
    let client = reqwest::Client::new();
    let req = client.request(reqwest::Method::GET, "https://api.track.toggl.com/api/v8/me").basic_auth(&config.toggl.api_token, Some("api_token"));
    let response = req.send().await?;
    println!("{:#?}", response.json::<Value>().await?);
    Ok(())
}
