//! Server-authoritative snapshot; every table, including empty/unknown tables,
//! must be returned. A second confirm certifies that the read did not drift.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const CHUNK_TABLES: usize = 15;

#[derive(Default)]
pub struct Snapshot {
    pub uid: Option<u64>,
    pub client_version: String,
    pub captured_at: u64,
    hashes: BTreeMap<String, String>,
    tables: BTreeMap<String, Value>,
    received_hashes: BTreeMap<String, String>,
    pub pull_chunks: usize,
    verified: bool,
    pub balance: Option<Value>,
}

fn hashes(body: &Value) -> Result<BTreeMap<String, String>> {
    success(body)?;
    let obj = body["hashes"]
        .as_object()
        .context("missing hashes object")?;
    ensure!(!obj.is_empty(), "empty confirm hash set");
    obj.iter()
        .map(|(name, val)| {
            ensure!(
                name.starts_with("user_")
                    && name.len() <= 128
                    && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
                "invalid table name"
            );
            let token = val.as_str().context("non-string table hash")?;
            ensure!(!token.is_empty(), "empty table hash");
            Ok((name.clone(), token.to_owned()))
        })
        .collect()
}

pub fn success(body: &Value) -> Result<()> {
    ensure!(body.is_object(), "response is not an object");
    if let Some(code) = body.get("code") {
        ensure!(
            code.as_i64() == Some(0),
            "server returned nonzero response code"
        );
    }
    Ok(())
}

impl Snapshot {
    pub fn identity(&mut self, uid: &str, version: &str) -> Result<()> {
        ensure!(
            !uid.is_empty() && uid.bytes().all(|c| c.is_ascii_digit()),
            "no logged-in game UID"
        );
        let uid: u64 = uid.parse().context("invalid game UID")?;
        ensure!(uid > 0 && uid <= i64::MAX as u64, "invalid game UID range");
        if let Some(previous) = self.uid {
            ensure!(previous == uid, "account changed during export");
        }
        if !self.client_version.is_empty() && !version.is_empty() {
            ensure!(
                self.client_version == version,
                "client version changed during export"
            );
        }
        self.uid = Some(uid);
        if !version.is_empty() {
            self.client_version = version.to_owned();
        }
        Ok(())
    }

    pub fn begin(&mut self, body: &Value, now: u64) -> Result<()> {
        ensure!(
            self.uid.is_some(),
            "confirm received without authenticated UID"
        );
        self.hashes = hashes(body)?;
        self.tables.clear();
        self.received_hashes.clear();
        self.pull_chunks = 0;
        self.verified = false;
        self.captured_at = now;
        Ok(())
    }

    pub fn batches(&self) -> Vec<Vec<String>> {
        self.hashes
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .chunks(CHUNK_TABLES)
            .map(|c| c.to_vec())
            .collect()
    }

    pub fn pull(&mut self, requested: &[String], body: &Value) -> Result<()> {
        success(body)?;
        let rows = body["tables"]
            .as_object()
            .context("missing tables object")?;
        let tokens = hashes(body)?;
        let want: BTreeSet<_> = requested.iter().cloned().collect();
        ensure!(
            !want.is_empty() && want.len() == requested.len(),
            "invalid pull request"
        );
        let got: BTreeSet<_> = rows.keys().cloned().collect();
        ensure!(
            !got.is_empty()
                && got.is_subset(&want)
                && tokens.keys().cloned().collect::<BTreeSet<_>>() == got,
            "pull table/hash coverage mismatch"
        );
        // Validate the whole chunk before changing the accumulated snapshot.
        for (name, value) in rows {
            ensure!(self.hashes.contains_key(name), "unconfirmed table returned");
            ensure!(!self.tables.contains_key(name), "duplicate table returned");
            let array = value.as_array().context("table is not an array")?;
            ensure!(
                array.iter().all(Value::is_object),
                "table row is not an object"
            );
        }
        self.tables
            .extend(rows.iter().map(|(n, r)| (n.clone(), r.clone())));
        self.received_hashes.extend(tokens);
        self.pull_chunks += 1;
        Ok(())
    }

    pub fn verify(&mut self, body: &Value) -> Result<()> {
        ensure!(
            self.tables.len() == self.hashes.len(),
            "incomplete full snapshot"
        );
        let current = hashes(body)?;
        ensure!(
            current == self.received_hashes,
            "server data changed during export; retry required"
        );
        self.hashes = current;
        self.verified = true;
        Ok(())
    }

    pub fn set_balance(&mut self, free: i32, paid: i32, total: i32) -> Result<()> {
        ensure!(
            free >= 0 && paid >= 0 && i64::from(free) + i64::from(paid) == i64::from(total),
            "invalid payment balance"
        );
        self.balance = Some(json!({"balance_free_gem": free, "balance_charge_gem": paid,
            "balance_total_gem": total}));
        Ok(())
    }

    pub fn archive(&self, now: u64) -> Result<Value> {
        ensure!(self.verified, "snapshot has not passed final confirm");
        let uid = self.uid.context("missing game UID")?;
        let balance = self
            .balance
            .as_ref()
            .context("payment balance has not been fetched")?;
        let mut account = Map::new();
        account.insert("userId".into(), json!(uid));
        account.insert("clientVersion".into(), json!(self.client_version));
        for (key, field) in [
            ("userName", "_userName"),
            ("rank", "_rank"),
            ("registeredAt", "_registeredAt"),
            ("country", "_country"),
            ("language", "_language"),
            ("comment", "_comment"),
        ] {
            account.insert(
                key.into(),
                self.tables
                    .get("user_profile")
                    .and_then(|t| t.get(0))
                    .and_then(|r| r.get(field))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
        account.insert(
            "bundleVersion".into(),
            self.tables
                .get("user_version")
                .and_then(|t| t.get(0))
                .and_then(|r| r.get("_bundleVersion"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        let count: usize = self
            .tables
            .values()
            .map(|t| t.as_array().unwrap().len())
            .sum();
        let empty = self
            .tables
            .values()
            .filter(|t| t.as_array().unwrap().is_empty())
            .count();
        Ok(json!({"format": "lilypad-export/1", "exportedAt": now,
            "capturedAt": self.captured_at, "account": account, "tables": self.tables,
            "officialHashes": self.hashes, "extras": {"paymentBalance": balance},
            "stats": {"tableCount": self.tables.len(), "rowCount": count,
                "emptyTables": empty, "pullChunks": self.pull_chunks, "replay": null,
                "fullSnapshotVerified": true}}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> Snapshot {
        let mut s = Snapshot::default();
        s.identity("1234567890123456", "6.0.0").unwrap();
        s.begin(
            &json!({"code":0,"hashes":{"user_item":"a","user_profile":"b","user_future":"c"}}),
            100,
        )
        .unwrap();
        s
    }

    #[test]
    fn full_archive_retains_unknown_empty_tables_and_exact_numbers() {
        let mut s = started();
        let body: Value = serde_json::from_str(r#"{"code":0,"tables":{"user_item":[{"_id":9223372036854775807,"_value":1.234567890123456789}],"user_profile":[{"_userName":"测试"}],"user_future":[]},"hashes":{"user_item":"a","user_profile":"b","user_future":"c"}}"#).unwrap();
        s.pull(&s.batches()[0], &body).unwrap();
        s.set_balance(25, 9, 34).unwrap();
        assert!(s.archive(101).is_err());
        s.verify(&json!({"hashes":{"user_item":"a","user_profile":"b","user_future":"c"}}))
            .unwrap();
        let out = s.archive(101).unwrap();
        assert_eq!(out["stats"]["tableCount"], 3);
        assert_eq!(out["stats"]["emptyTables"], 1);
        assert_eq!(out["account"]["userId"], 1234567890123456u64);
        assert!(out.to_string().contains("1.234567890123456789"));
        assert!(out.to_string().contains("9223372036854775807"));
    }

    #[test]
    fn fails_closed_for_missing_tables_wrong_rows_and_server_errors() {
        let mut s = started();
        for body in [
            json!({"tables":{"user_unrequested":[]},"hashes":{"user_unrequested":"a"}}),
            json!({"tables":{"user_item":[null],"user_profile":[],"user_future":[]},
                "hashes":{"user_item":"a","user_profile":"b","user_future":"c"}}),
            json!({"code":2,"tables":{},"hashes":{}}),
        ] {
            assert!(s.pull(&s.batches()[0], &body).is_err());
        }
        assert!(s.verify(&json!({"hashes":{"user_item":"a"}})).is_err());
        assert!(s.archive(0).is_err());
    }

    #[test]
    fn detects_account_switch_duplicates_and_drift() {
        let mut s = started();
        assert!(s.identity("abcdef0123456789abcdef0123456789", "").is_err());
        assert!(s.identity("2", "").is_err());
        assert!(s.identity("0", "").is_err());
        let body = json!({"tables":{"user_item":[],"user_profile":[],"user_future":[]},
            "hashes":{"user_item":"a","user_profile":"b","user_future":"c"}});
        s.pull(&s.batches()[0], &body).unwrap();
        assert!(s.pull(&s.batches()[0], &body).is_err());
        assert!(s
            .verify(&json!({"hashes":{"user_item":"changed","user_profile":"b","user_future":"c"}}))
            .is_err());
        assert!(s.archive(0).is_err());
        assert!(s.set_balance(5, 6, 12).is_err());
    }

    #[test]
    fn batches_cover_all_confirmed_tables_without_fixed_table_count() {
        let mut s = Snapshot::default();
        s.identity("1", "v").unwrap();
        let h: Map<String, Value> = (0..247)
            .map(|i| (format!("user_{i}"), json!("hash")))
            .collect();
        s.begin(&json!({"hashes":h}), 0).unwrap();
        assert_eq!(s.batches().len(), 17);
        assert_eq!(
            s.batches().iter().flatten().collect::<BTreeSet<_>>().len(),
            247
        );
    }

    #[test]
    fn size_limited_pull_must_be_continued_until_every_table_arrives() {
        let mut s = started();
        s.pull(
            &s.batches()[0],
            &json!({"tables":{"user_item":[]},"hashes":{"user_item":"a"}}),
        )
        .unwrap();
        assert!(s
            .verify(&json!({"hashes":{"user_item":"a","user_profile":"b","user_future":"c"}}))
            .is_err());
        s.pull(&["user_profile".into(), "user_future".into()], &json!({"tables":{"user_profile":[],"user_future":[]},"hashes":{"user_profile":"b","user_future":"c"}})).unwrap();
        s.verify(&json!({"hashes":{"user_item":"a","user_profile":"b","user_future":"c"}}))
            .unwrap();
        assert!(s.archive(101).is_err()); // Full tables still require payment data.
        s.set_balance(0, 0, 0).unwrap();
        assert_eq!(s.archive(101).unwrap()["stats"]["pullChunks"], 2);
    }
}
