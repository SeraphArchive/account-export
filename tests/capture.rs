//! Optional private capture regression. No captured account data is source.
use hbr_account_export::{snapshot::Snapshot, storage::write_archive};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[test]
#[ignore = "requires a private decoded capture and output directory"]
fn decoded_startup_capture_exports_complete_tables() {
    let path = std::env::var_os("HBR_EXPORT_CAPTURE").expect("set HBR_EXPORT_CAPTURE");
    let output = PathBuf::from(
        std::env::var_os("HBR_EXPORT_TEST_OUTPUT").expect("set HBR_EXPORT_TEST_OUTPUT"),
    );
    let records: Vec<Value> = BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
        .collect();
    let mut snapshot = Snapshot::default();
    let mut uid = String::new();
    let mut version = String::new();
    let mut confirm: Option<Value> = None;
    let mut requested = vec![];
    let mut complete = false;
    let mut balance = None;
    for record in records {
        if let Some(v) = record["headers"]["x-player-id"].as_str() {
            if v.bytes().all(|c| c.is_ascii_digit()) {
                uid = v.to_owned();
            }
        }
        if let Some(v) = record["headers"]["x-client-version"].as_str() {
            version = v.to_owned();
        }
        let body = if let Some(s) = record["body"].as_str() {
            match serde_json::from_str(s) {
                Ok(v) => v,
                Err(_) => continue,
            }
        } else {
            record["body"].clone()
        };
        let route = record["route"].as_str().unwrap_or_default();
        let kind = record["kind"].as_str().unwrap_or_default();
        if kind == "RESP" && route.starts_with("/v1.0/payment/balance") {
            let entry = body.get("entry").unwrap_or(&body);
            let number = |key| -> i32 {
                let v = &entry[key];
                v.as_i64()
                    .map(|n| i32::try_from(n).unwrap())
                    .unwrap_or_else(|| v.as_str().unwrap().parse().unwrap())
            };
            balance = Some((
                number("balance_free_gem"),
                number("balance_charge_gem"),
                number("balance_total_gem"),
            ));
        }
        if complete {
            continue;
        }
        if kind == "RESP" && route == "/api/user/confirm" && confirm.is_none() {
            snapshot.identity(&uid, &version).unwrap();
            snapshot.begin(&body, 1).unwrap();
            confirm = Some(body);
        } else if kind == "REQ" && route == "/api/user/pull" && confirm.is_some() {
            requested = body["tables"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
        } else if kind == "RESP" && route == "/api/user/pull" && confirm.is_some() {
            snapshot.identity(&uid, &version).unwrap();
            snapshot.pull(&requested, &body).unwrap();
            // Offline evidence only: production obtains a new final confirm.
            complete = snapshot.verify(confirm.as_ref().unwrap()).is_ok();
        }
    }
    assert!(
        complete,
        "capture did not contain a complete stable startup snapshot"
    );
    let (free, paid, total) = balance.expect("capture must contain payment balance");
    snapshot.set_balance(free, paid, total).unwrap();
    let doc = snapshot.archive(2).unwrap();
    let dest = write_archive(&output, &doc, snapshot.uid.unwrap(), 2).unwrap();
    println!(
        "capture regression passed: {} tables, {} rows, {} response chunks; archive: {}",
        doc["stats"]["tableCount"],
        doc["stats"]["rowCount"],
        doc["stats"]["pullChunks"],
        dest.display()
    );
}
