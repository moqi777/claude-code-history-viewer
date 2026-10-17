//! The vector file: `<data dir>/vectors/<MODEL_ID>.sqlite3`.
//!
//! Vectors are derived data. They live apart from `kb.db` so that file stays
//! small and so deleting this one never loses anything. Each row is keyed by
//! `(source, session_id, key)` plus the SHA-256 of the text it was computed
//! from: a row whose text did not change is never computed again, not even
//! after `--kb-reindex` or an import that renumbers file ids.
//!
//! A vector is stored as int8 plus one f32 scale (a quarter of the size; the
//! measured ranking is the same as with f32).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

use super::chunk::Chunk;
use super::{CHUNKER_VERSION, DIM, MODEL_ID};
use crate::kb::paths::DataPaths;

pub fn dir(data: &DataPaths) -> PathBuf {
    data.root().join("vectors")
}

pub fn path(data: &DataPaths) -> PathBuf {
    dir(data).join(format!("{MODEL_ID}.sqlite3"))
}

pub type Key = (String, String, String);

pub struct Store {
    conn: Connection,
}

fn err(what: &str, e: impl std::fmt::Display) -> String {
    format!("{what}: {e}")
}

pub fn quantize(v: &[f32]) -> (f32, Vec<i8>) {
    let max = v.iter().fold(0f32, |a, b| a.max(b.abs())).max(1e-9);
    let scale = max / 127.0;
    (
        scale,
        v.iter()
            .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8)
            .collect(),
    )
}

fn as_bytes(v: &[i8]) -> Vec<u8> {
    v.iter().map(|x| x.to_ne_bytes()[0]).collect()
}

impl Store {
    /// Open for writing, creating the file. A file made by another model or
    /// another way of cutting text is emptied: its vectors are no use.
    pub fn open(path: &Path, model_sha: &str) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err("cannot create vectors dir", e))?;
        }
        let conn = Connection::open(path).map_err(|e| err("cannot open the vector file", e))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| err("busy_timeout", e))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS chunks (
               source TEXT NOT NULL, session_id TEXT NOT NULL, key TEXT NOT NULL,
               kind TEXT NOT NULL, sha TEXT NOT NULL, scale REAL NOT NULL, vec BLOB NOT NULL,
               PRIMARY KEY (source, session_id, key)
             );",
        )
        .map_err(|e| err("cannot prepare the vector file", e))?;
        let store = Self { conn };
        let want = Self::expected_meta(model_sha);
        if store.meta()? != want {
            store
                .conn
                .execute_batch("DELETE FROM chunks; DELETE FROM meta;")
                .map_err(|e| err("cannot reset the vector file", e))?;
            for (k, v) in &want {
                store
                    .conn
                    .execute(
                        "INSERT INTO meta(key, value) VALUES (?1, ?2)",
                        params![k, v],
                    )
                    .map_err(|e| err("cannot write meta", e))?;
            }
        }
        Ok(store)
    }

    /// Open for reading only. `Ok(None)` when there is no usable file (missing,
    /// or made for another model / cutting rule).
    pub fn open_ro(path: &Path, model_sha: &str) -> Result<Option<Self>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| err("cannot open the vector file", e))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| err("busy_timeout", e))?;
        let store = Self { conn };
        match store.meta() {
            Ok(m) if m == Self::expected_meta(model_sha) => Ok(Some(store)),
            Ok(_) => Ok(None),
            Err(e) => Err(format!("the vector file is damaged ({e})")),
        }
    }

    fn expected_meta(model_sha: &str) -> Vec<(String, String)> {
        let mut m = vec![
            ("chunker_version".to_string(), CHUNKER_VERSION.to_string()),
            ("dim".to_string(), DIM.to_string()),
            ("model_id".to_string(), MODEL_ID.to_string()),
            ("model_sha".to_string(), model_sha.to_string()),
        ];
        m.sort();
        m
    }

    fn meta(&self) -> Result<Vec<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM meta ORDER BY key")
            .map_err(|e| err("meta", e))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| err("meta", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| err("meta", e))?;
        Ok(rows)
    }

    pub fn count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))
            .map_or(0, |n| usize::try_from(n).unwrap_or(0))
    }

    /// Every stored key with the hash of the text it was computed from.
    pub fn keys(&self) -> Result<HashMap<Key, String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, session_id, key, sha FROM chunks")
            .map_err(|e| err("keys", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    (
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ),
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| err("keys", e))?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| err("keys", e))?;
        Ok(rows)
    }

    /// Write a batch in one transaction.
    pub fn put(&mut self, rows: &[(&Chunk, Vec<f32>)]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| err("transaction", e))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO chunks(source, session_id, key, kind, sha, scale, vec)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(|e| err("insert", e))?;
            for (chunk, v) in rows {
                if v.len() != DIM {
                    return Err(format!("vector has {} numbers, expected {DIM}", v.len()));
                }
                let (scale, q) = quantize(v);
                stmt.execute(params![
                    chunk.source,
                    chunk.session_id,
                    chunk.key,
                    chunk.kind,
                    chunk.sha,
                    f64::from(scale),
                    as_bytes(&q)
                ])
                .map_err(|e| err("insert", e))?;
            }
        }
        tx.commit().map_err(|e| err("commit", e))
    }

    pub fn remove(&mut self, keys: &[Key]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| err("transaction", e))?;
        {
            let mut stmt = tx
                .prepare("DELETE FROM chunks WHERE source=?1 AND session_id=?2 AND key=?3")
                .map_err(|e| err("delete", e))?;
            for (s, id, k) in keys {
                stmt.execute(params![s, id, k])
                    .map_err(|e| err("delete", e))?;
            }
        }
        tx.commit().map_err(|e| err("commit", e))
    }

    /// Everything, for searching.
    pub fn load(&self) -> Result<Matrix, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, session_id, kind, key, scale, vec FROM chunks")
            .map_err(|e| err("load", e))?;
        let mut m = Matrix::default();
        let mut rows = stmt.query([]).map_err(|e| err("load", e))?;
        while let Some(r) = rows.next().map_err(|e| err("load", e))? {
            let blob: Vec<u8> = r.get(5).map_err(|e| err("load", e))?;
            if blob.len() != DIM {
                continue;
            }
            m.owner.push((
                r.get(0).map_err(|e| err("load", e))?,
                r.get(1).map_err(|e| err("load", e))?,
                r.get(2).map_err(|e| err("load", e))?,
                r.get(3).map_err(|e| err("load", e))?,
            ));
            m.scale
                .push(r.get::<_, f64>(4).map_err(|e| err("load", e))? as f32);
            m.data.extend(blob.iter().map(|b| i8::from_ne_bytes([*b])));
        }
        Ok(m)
    }

    /// A consistent copy of the file (the `SQLite` backup interface), for export.
    pub fn backup_to(&self, dest: &Path) -> Result<(), String> {
        let mut out = Connection::open(dest).map_err(|e| err("backup target", e))?;
        let backup =
            rusqlite::backup::Backup::new(&self.conn, &mut out).map_err(|e| err("backup", e))?;
        backup
            .run_to_completion(256, std::time::Duration::from_millis(5), None)
            .map_err(|e| err("backup", e))?;
        drop(backup);
        out.execute_batch("PRAGMA journal_mode=DELETE;")
            .map_err(|e| err("backup", e))?;
        Ok(())
    }
}

#[derive(Default)]
pub struct Matrix {
    /// `(source, session id, kind, key)` of each row.
    pub owner: Vec<(String, String, String, String)>,
    pub scale: Vec<f32>,
    pub data: Vec<i8>,
}

impl Matrix {
    pub fn len(&self) -> usize {
        self.owner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.owner.is_empty()
    }

    /// Cosine similarity of the (unit length) query with every row.
    pub fn scores(&self, query: &[f32]) -> Vec<f32> {
        (0..self.len())
            .map(|i| {
                let row = &self.data[i * DIM..(i + 1) * DIM];
                let dot: f32 = row.iter().zip(query).map(|(a, b)| f32::from(*a) * b).sum();
                dot * self.scale[i]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::semantic::chunk::text_sha;

    fn chunk(key: &str, text: &str) -> Chunk {
        Chunk {
            source: "claude".into(),
            session_id: "s1".into(),
            key: key.into(),
            kind: "user",
            text: text.into(),
            sha: text_sha(text),
        }
    }

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0f32; DIM];
        v[i % DIM] = 1.0;
        v
    }

    #[test]
    fn put_load_score_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.sqlite3");
        let mut store = Store::open(&path, "sha-a").unwrap();
        let (first, second) = (chunk("m:1:0", "alpha"), chunk("m:2:0", "beta"));
        store.put(&[(&first, unit(3)), (&second, unit(9))]).unwrap();
        assert_eq!(store.count(), 2);
        let m = store.load().unwrap();
        let sc = m.scores(&unit(3));
        let best = sc
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .unwrap()
            .0;
        assert_eq!(m.owner[best].3, "m:1:0");
        assert!(sc[best] > 0.99 && sc.iter().filter(|x| **x > 0.5).count() == 1);
        let keys = store.keys().unwrap();
        assert_eq!(
            keys[&("claude".into(), "s1".into(), "m:1:0".into())],
            first.sha
        );
    }

    #[test]
    fn remove_and_replace() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(&dir.path().join("v.sqlite3"), "x").unwrap();
        let a = chunk("m:1:0", "alpha");
        s.put(&[(&a, unit(1))]).unwrap();
        let a2 = chunk("m:1:0", "alpha changed");
        s.put(&[(&a2, unit(2))]).unwrap();
        assert_eq!(s.count(), 1);
        assert_eq!(s.keys().unwrap().values().next().unwrap(), &a2.sha);
        s.remove(&[("claude".into(), "s1".into(), "m:1:0".into())])
            .unwrap();
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn another_model_empties_the_file_and_ro_refuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.sqlite3");
        let mut s = Store::open(&p, "model-1").unwrap();
        s.put(&[(&chunk("t", "titel text"), unit(1))]).unwrap();
        drop(s);
        assert!(Store::open_ro(&p, "model-1").unwrap().is_some());
        assert!(Store::open_ro(&p, "model-2").unwrap().is_none());
        let s2 = Store::open(&p, "model-2").unwrap();
        assert_eq!(s2.count(), 0);
        assert!(Store::open_ro(&dir.path().join("none"), "x")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_file_that_is_not_a_database_is_reported_as_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.sqlite3");
        std::fs::write(
            &p,
            b"this is not a sqlite file at all, just some text of a decent length...",
        )
        .unwrap();
        let err = Store::open_ro(&p, "x").err().unwrap();
        assert!(err.contains("damaged"), "{err}");
        assert!(Store::open(&p, "x").is_err());
    }

    #[test]
    fn backup_is_a_usable_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(&dir.path().join("v.sqlite3"), "x").unwrap();
        s.put(&[(&chunk("t", "titel text"), unit(1))]).unwrap();
        let copy = dir.path().join("copy.sqlite3");
        s.backup_to(&copy).unwrap();
        assert_eq!(Store::open_ro(&copy, "x").unwrap().unwrap().count(), 1);
    }

    #[test]
    fn quantizing_keeps_the_direction() {
        let mut v: Vec<f32> = (0..DIM).map(|i| ((i * 7 % 13) as f32) - 6.0).collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut v {
            *x /= n;
        }
        let (scale, q) = quantize(&v);
        let back: f32 = q
            .iter()
            .zip(&v)
            .map(|(a, b)| f32::from(*a) * scale * b)
            .sum();
        assert!(back > 0.999, "{back}");
    }
}
