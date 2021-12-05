#![feature(entry_insert)]

use std::{collections::{BTreeMap, HashMap}, error::Error, str::FromStr};
use num_rational::Rational64;
use reqwest::Url;
use serde::Deserialize;
use tokio::{fs::File, io::AsyncReadExt};
use serde_json::Value;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
extern crate chrono_tz;
use chrono_tz::Tz;
use kv::{Codec, Json, Store};

mod accounting;
mod timetracking;

#[derive(Deserialize)]
struct Toggl {
    api_token: String,
}

#[derive(Deserialize)]
struct FreeFinance {
    app_key: String,
}

#[derive(Deserialize)]
struct Config {
    toggl: Toggl,
    freefinance: FreeFinance,
    client: String,
    rate: i64
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap_or_else(|| NaiveDate::from_ymd(year + 1, 1, 1)).pred().day()
}

fn ratio_to_string(r: Rational64) -> String {
    let integer_part = r.to_integer();
    let decimal_part = ((r - integer_part)*100).round().to_integer();
    format!("{}.{:02}", integer_part, decimal_part)
}

fn with_confirm<T>(msg: &str, default: Option<bool>, f: impl Fn() -> T) -> Option<T> {
    println!("{} {}", msg, match default {
        Some(true) => "[Y/n]",
        Some(false) => "[y/N]",
        None => "[y/n]",
    });
    
    loop {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        line = line.to_lowercase();
        if line.len() > 0 {
            let first = line.chars().take(1).last().unwrap();
            if first == 'y' {
                return Some(f());
            } else if first == 'n' {
                break;
            }
        } else {
            match default {
                Some(true) => return Some(f()),
                Some(false) => break,
                None => {},
            };
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut f = File::open("conf/config.toml").await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    let config = toml::from_slice::<Config>(&buf)?;
    let client = reqwest::Client::new();

    // let url = accounting::freefinance::oauth_url(true, config.freefinance.app_key.as_str(), "", "");
    // println!("{}", url);
    //return Ok(());

    let req = client.request(reqwest::Method::GET, "https://api.track.toggl.com/api/v8/me").basic_auth(&config.toggl.api_token, Some("api_token"));
    let response = req.send().await?;
    let response = response.json::<Value>().await?;
    let tz = response["data"]["timezone"].as_str().ok_or("No timezone set")?;
    let tz: Tz = tz.parse()?;

    let local: DateTime<Local> = Local::now();
    let begin = tz.ymd(local.year(), local.month(), 1).and_hms(0, 0, 0);
    let begin = begin - Duration::weeks(4*3);
    let begin = begin.format("%+").to_string();
    let end = tz.ymd(local.year(), local.month(), last_day_of_month(local.year(), local.month())).and_hms(23, 59, 59);
    let end = end.format("%+").to_string();
    println!("From {begin} to {end}", begin=begin, end=end);

    let kv_cfg = kv::Config::new("./store/");

    let store = Store::new(kv_cfg)?;

    let billed_bucket = store.bucket::<kv::Integer, Json<bool>>(Some("billed"))?;

    let req = client.request(reqwest::Method::GET, Url::parse_with_params("https://api.track.toggl.com/api/v8/time_entries", &[
        ("start_date", begin.to_string()),
        ("end_date", end.to_string()),
    ])?).basic_auth(&config.toggl.api_token, Some("api_token"));

    let response = req.send().await?;
    let response = response.json::<Vec<Value>>().await?;

    let mut projects = HashMap::new(); // Project ID -> Client ID
    let mut clients = HashMap::new(); // Client ID -> Client Name 
    
    let mut summary = BTreeMap::new();

    let mut to_bill = Vec::new();

    for entry in response {
        let id = entry["id"].as_i64().expect("Expected time entry ID");

        if billed_bucket.get(kv::Integer::from(id as u64)).expect("Could not read store").map(|x| x.into_inner()).unwrap_or(false) {
            continue;
        }

        let description = entry["description"].as_str().unwrap_or("Other").trim();
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
            to_bill.push(id);
        }
    }

    let mut sorted_by_duration: Vec<_> = summary.iter().map(|v| {
        let hours = Rational64::new(*v.1, 3600);
        let rounded_hours = (hours * 100).round() / 100;
        (hours, rounded_hours, v.0)
    }).collect();
    sorted_by_duration.sort_by(|a, b| b.partial_cmp(a).unwrap());

    let deviation = sorted_by_duration.iter().map(|x| (x.1 - x.0) * config.rate).sum::<Rational64>();
    dbg!(deviation);

    let rate = Rational64::from(config.rate) / 100;
    let mut total = Rational64::from(0);
    let mut total_hours = Rational64::from(0);
    for (i, x) in sorted_by_duration.iter().enumerate() {
        let rounded_hours = x.1;
        let description = x.2;
        let price = rate * rounded_hours;
        total += price;
        total_hours += rounded_hours;
        println!("{i}: {description} ({duration} hrs)",
            i=i+1,
            description=description,
            duration=ratio_to_string(rounded_hours),
        );
    }
    println!("");
    println!("Total hours: {}", ratio_to_string(total_hours));

    with_confirm("Commit changes to database?", Some(false), || {
        billed_bucket.transaction(|tx| {
            for id in &to_bill {
                tx.set(kv::Integer::from(*id as u64), Json(true))?;
            }
            Ok(())
        })?;
        Ok::<_, Box<dyn Error>>(())
    }).transpose()?;
    
    Ok(())
}
