#![feature(entry_insert)]

use std::{ascii::AsciiExt, collections::{BTreeMap, HashMap}, error::Error, ops::Add, str::FromStr};
use num_rational::Rational64;
use reqwest::Url;
use serde::Deserialize;
use tokio::{fs::File, io::AsyncReadExt};
use serde_json::Value;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use chrono_tz::Tz;

#[derive(Deserialize)]
struct Toggl {
    api_token: String,
}

#[derive(Deserialize)]
struct Config {
    toggl: Toggl,
    client: String,
    rate: i64
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap_or(NaiveDate::from_ymd(year + 1, 1, 1)).pred().day()
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
    let response = response.json::<Value>().await?;
    let tz = response["data"]["timezone"].as_str().ok_or("No timezone set")?;
    let tz: Tz = tz.parse()?;

    let local: DateTime<Local> = Local::now();
    let begin = tz.ymd(local.year(), local.month(), 1).and_hms(0, 0, 0);
    let begin = begin.format("%+").to_string();
    let end = tz.ymd(local.year(), local.month(), last_day_of_month(local.year(), local.month())).and_hms(23, 59, 59);
    let end = end.format("%+").to_string();
    println!("From {begin} to {end}", begin=begin, end=end);

    let req = client.request(reqwest::Method::GET, Url::parse_with_params("https://api.track.toggl.com/api/v8/time_entries", &[
        ("start_date", begin.to_string()),
        ("end_date", end.to_string()),
    ])?).basic_auth(&config.toggl.api_token, Some("api_token"));

    let response = req.send().await?;
    let response = response.json::<Vec<Value>>().await?;

    let mut projects = HashMap::new(); // Project ID -> Client ID
    let mut clients = HashMap::new(); // Client ID -> Client Name 
    
    let mut summary = BTreeMap::new();
    for entry in response {
        let description = entry["description"].as_str().unwrap_or("Other");
        let duration = entry["duration"].as_i64().expect("Expected duration");

        let project_id = match entry["pid"].as_i64() {
            Some(x) => x,
            None => {
                println!("warning: missing project id");
                continue;
            }
        };

        let client_id = match projects.entry(project_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let req = client.request(reqwest::Method::GET, Url::from_str("https://api.track.toggl.com/api/v8/projects/")?.join(&project_id.to_string())?).basic_auth(&config.toggl.api_token, Some("api_token"));
                let response = req.send().await?;
                let response = response.json::<Value>().await?;
                let client_id = response["data"]["cid"].as_i64();
                let client_id = match client_id {
                    Some(cid) => cid,
                    None => { println!("warning: ignoring {}/{} because it has no client ID", description, response["data"]["name"].to_string()); continue; }
                };

                *entry.insert(client_id)
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                *entry.get()
            }
        };

        let client_name = match clients.entry(client_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let req = client.request(reqwest::Method::GET, Url::from_str("https://api.track.toggl.com/api/v8/clients/")?.join(&client_id.to_string())?).basic_auth(&config.toggl.api_token, Some("api_token"));
                let response = req.send().await?;
                let response = response.json::<Value>().await?;
                entry.insert(response["data"]["name"].as_str().expect("Client has no name").to_string()).to_owned()
            },
            std::collections::hash_map::Entry::Occupied(entry) => {
                entry.get().to_owned()
            }
        };

        if duration < 0 {
            println!("warning: duration negative");
            continue;
        }

        if client_name == config.client {
            *summary.entry(description.to_string()).or_insert(0) += duration;
        }
    }

    let mut sorted_by_duration: Vec<_> = summary.iter().map(|v| {
        let hours = Rational64::new(*v.1, 3600);
        let price = hours * config.rate;
        (hours, price, v.0)
    }).collect();
    sorted_by_duration.sort_by(|a, b| a.partial_cmp(b).unwrap());

    dbg!(sorted_by_duration);
    Ok(())
}
