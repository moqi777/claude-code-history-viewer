//! The model files: where they live, whether they are the right ones, and the
//! one place in the program that goes onto the network (only when the user
//! presses "download").
//!
//! The expected size and SHA-256 of every file are written into the program.
//! A file that does not match is deleted when it comes from a download and
//! refused when it comes from a folder the user picked.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::MODEL_DIR;
use crate::kb::paths::DataPaths;

pub struct FileSpec {
    /// Name inside the models folder.
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// Path of the file in the Hugging Face / `ModelScope` repository.
    pub remote: &'static str,
    /// Other names a user-picked folder may use for it.
    pub local_names: &'static [&'static str],
}

pub struct ModelSpec {
    pub files: &'static [FileSpec],
    pub hf_repo: &'static str,
    pub ms_repo: &'static str,
}

/// `onnx-community/bge-m3-ONNX`, int8. The two sources serve the same bytes
/// (checked: same size and SHA-256 on both).
pub const SPEC: ModelSpec = ModelSpec {
    files: &[
        FileSpec {
            name: "model.onnx",
            size: 568_479_395,
            sha256: "2237f770aad5c71bbc1fc2d361a57f9a37400574cc9eff32626f0cdb49234730",
            remote: "onnx/model_int8.onnx",
            local_names: &["model.onnx", "model_int8.onnx", "onnx/model_int8.onnx"],
        },
        FileSpec {
            name: "tokenizer.json",
            size: 17_082_799,
            sha256: "249df0778f236f6ece390de0de746838ef25b9d6954b68c2ee71249e0a9d8fd4",
            remote: "tokenizer.json",
            local_names: &["tokenizer.json"],
        },
    ],
    hf_repo: "onnx-community/bge-m3-ONNX",
    ms_repo: "onnx-community/bge-m3-ONNX",
};

/// The SHA-256 of the model itself: what the vector file records.
pub fn model_sha() -> &'static str {
    SPEC.files[0].sha256
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Modelscope,
    Huggingface,
}

impl Source {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "modelscope" => Some(Self::Modelscope),
            "huggingface" => Some(Self::Huggingface),
            _ => None,
        }
    }

    pub fn url(self, spec: &ModelSpec, file: &FileSpec) -> String {
        match self {
            Self::Huggingface => {
                format!(
                    "https://huggingface.co/{}/resolve/main/{}",
                    spec.hf_repo, file.remote
                )
            }
            Self::Modelscope => format!(
                "https://modelscope.cn/api/v1/models/{}/repo?Revision=master&FilePath={}",
                spec.ms_repo, file.remote
            ),
        }
    }
}

pub fn dir(data: &DataPaths) -> PathBuf {
    data.root().join("models").join(MODEL_DIR)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelStatus {
    Missing,
    Corrupt { file: String, reason: String },
    Ready,
}

#[derive(Serialize, Deserialize, Default)]
struct Verified {
    files: Vec<VerifiedFile>,
}

#[derive(Serialize, Deserialize, Clone)]
struct VerifiedFile {
    name: String,
    size: u64,
    mtime_ns: u128,
    sha256: String,
}

fn mtime_ns(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Whether the model files are there and right. Cheap when nothing changed
/// since the last full check (size and modification time are compared with a
/// note kept from then); a changed file is hashed again (a couple of seconds
/// for the 570 MB model).
pub fn check(data: &DataPaths, spec: &ModelSpec) -> ModelStatus {
    let root = dir(data);
    let present: Vec<bool> = spec
        .files
        .iter()
        .map(|f| root.join(f.name).is_file())
        .collect();
    if present.iter().all(|p| !p) {
        return ModelStatus::Missing;
    }
    let note_path = root.join("verified.json");
    let mut note: Verified = std::fs::read_to_string(&note_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut changed = false;
    for (f, there) in spec.files.iter().zip(&present) {
        if !there {
            return ModelStatus::Corrupt {
                file: f.name.into(),
                reason: "missing".into(),
            };
        }
        let path = root.join(f.name);
        let Ok(meta) = std::fs::metadata(&path) else {
            return ModelStatus::Corrupt {
                file: f.name.into(),
                reason: "unreadable".into(),
            };
        };
        if meta.len() != f.size {
            return ModelStatus::Corrupt {
                file: f.name.into(),
                reason: format!("size {} instead of {}", meta.len(), f.size),
            };
        }
        let stamp = mtime_ns(&meta);
        let known = note.files.iter().any(|v| {
            v.name == f.name && v.size == meta.len() && v.mtime_ns == stamp && v.sha256 == f.sha256
        });
        if known {
            continue;
        }
        match sha256_file(&path) {
            Ok(sha) if sha == f.sha256 => {
                note.files.retain(|v| v.name != f.name);
                note.files.push(VerifiedFile {
                    name: f.name.into(),
                    size: meta.len(),
                    mtime_ns: stamp,
                    sha256: sha,
                });
                changed = true;
            }
            Ok(_) => {
                return ModelStatus::Corrupt {
                    file: f.name.into(),
                    reason: "the SHA-256 does not match".into(),
                }
            }
            Err(e) => {
                return ModelStatus::Corrupt {
                    file: f.name.into(),
                    reason: e,
                }
            }
        }
    }
    if changed {
        if let Ok(text) = serde_json::to_string(&note) {
            let _ = std::fs::write(&note_path, text);
        }
    }
    ModelStatus::Ready
}

/// Remove the model files (and the note about them).
pub fn delete(data: &DataPaths) -> Result<(), String> {
    let root = dir(data);
    if root.is_dir() {
        std::fs::remove_dir_all(&root)
            .map_err(|e| format!("cannot delete {}: {e}", root.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub file: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

const ATTEMPTS: u32 = 3;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(std::time::Duration::from_secs(20))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent("AgentKB")
        .build()
}

/// Download every file of `spec` that is not already right. `progress` gets
/// called a few times a second; `cancel` is checked between chunks. A file
/// only appears under its final name once its size and SHA-256 match.
pub fn download(
    data: &DataPaths,
    spec: &ModelSpec,
    source: Source,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), String> {
    let root = dir(data);
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let total: u64 = spec.files.iter().map(|f| f.size).sum();
    let mut finished: u64 = 0;
    for f in spec.files {
        let final_path = root.join(f.name);
        if final_path.is_file()
            && std::fs::metadata(&final_path).is_ok_and(|m| m.len() == f.size)
            && sha256_file(&final_path).is_ok_and(|s| s == f.sha256)
        {
            finished += f.size;
            continue;
        }
        let url = source.url(spec, f);
        let part = root.join(format!("{}.part", f.name));
        let mut last_err = String::new();
        let mut ok = false;
        for attempt in 1..=ATTEMPTS {
            match fetch(&url, &part, f, cancel, &mut |done| {
                progress(&Progress {
                    file: f.name.into(),
                    done_bytes: finished + done,
                    total_bytes: total,
                });
            }) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(FetchError::Cancelled) => return Err("cancelled".into()),
                Err(FetchError::Mismatch(m)) => {
                    let _ = std::fs::remove_file(&part);
                    return Err(format!(
                        "the downloaded {} does not match what was expected ({m}); it was deleted",
                        f.name
                    ));
                }
                Err(FetchError::Network(m)) => {
                    last_err = m;
                    if attempt < ATTEMPTS {
                        std::thread::sleep(std::time::Duration::from_secs(u64::from(attempt) * 2));
                    }
                }
            }
        }
        if !ok {
            return Err(format!("download of {} failed: {last_err}", f.name));
        }
        std::fs::rename(&part, &final_path)
            .map_err(|e| format!("cannot move {} into place: {e}", f.name))?;
        finished += f.size;
    }
    Ok(())
}

enum FetchError {
    Cancelled,
    Network(String),
    Mismatch(String),
}

fn fetch(
    url: &str,
    part: &Path,
    spec: &FileSpec,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), FetchError> {
    let have = std::fs::metadata(part).map_or(0, |m| m.len());
    let mut offset = if have > spec.size { 0 } else { have };
    let mut req = agent().get(url);
    if offset > 0 {
        req = req.set("Range", &format!("bytes={offset}-"));
    }
    let resp = req.call().map_err(|e| FetchError::Network(e.to_string()))?;
    // A server that ignores Range answers 200 with the whole file: start over.
    if offset > 0 && resp.status() != 206 {
        offset = 0;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(part)
        .map_err(|e| FetchError::Network(format!("cannot write {}: {e}", part.display())))?;
    let mut hasher = Sha256::new();
    if offset > 0 {
        // Hash what is already there, then continue.
        let mut old = File::open(part).map_err(|e| FetchError::Network(e.to_string()))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut left = offset;
        while left > 0 {
            let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            let n = old
                .read(&mut buf[..want])
                .map_err(|e| FetchError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| FetchError::Network(e.to_string()))?;
    }
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = offset;
    let mut last_report = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(FetchError::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| FetchError::Network(e.to_string()))?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if last_report.elapsed() >= std::time::Duration::from_millis(250) {
            on_bytes(done);
            last_report = std::time::Instant::now();
        }
    }
    file.flush()
        .map_err(|e| FetchError::Network(e.to_string()))?;
    on_bytes(done);
    if done < spec.size {
        return Err(FetchError::Network(format!(
            "the connection ended after {done} of {} bytes",
            spec.size
        )));
    }
    let sha = hex(&hasher.finalize());
    if done != spec.size || sha != spec.sha256 {
        return Err(FetchError::Mismatch(format!("{done} bytes, SHA-256 {sha}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// From a folder the user picked
// ---------------------------------------------------------------------------

/// Copy the model files out of `folder` (any layout in `local_names`), but only
/// files whose size and SHA-256 are exactly the expected ones.
pub fn import_folder(data: &DataPaths, spec: &ModelSpec, folder: &Path) -> Result<(), String> {
    let root = dir(data);
    let mut picked: Vec<(&FileSpec, PathBuf)> = Vec::new();
    for f in spec.files {
        let found = f
            .local_names
            .iter()
            .map(|n| folder.join(n))
            .find(|p| p.is_file());
        let Some(path) = found else {
            return Err(format!("{} was not found in that folder", f.name));
        };
        let meta =
            std::fs::metadata(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if meta.len() != f.size {
            return Err(format!(
                "{} is not the expected file (size {} instead of {})",
                path.display(),
                meta.len(),
                f.size
            ));
        }
        let sha = sha256_file(&path)?;
        if sha != f.sha256 {
            return Err(format!(
                "{} is not the expected file (SHA-256 differs)",
                path.display()
            ));
        }
        picked.push((f, path));
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    for (f, src) in picked {
        let part = root.join(format!("{}.part", f.name));
        std::fs::copy(&src, &part).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
        std::fs::rename(&part, root.join(f.name))
            .map_err(|e| format!("cannot move {} into place: {e}", f.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    fn tiny(content: &'static [u8]) -> (ModelSpec, String) {
        let sha = hex(&Sha256::digest(content));
        let sha: &'static str = Box::leak(sha.clone().into_boxed_str());
        let files: &'static [FileSpec] = Box::leak(Box::new([FileSpec {
            name: "model.onnx",
            size: content.len() as u64,
            sha256: sha,
            remote: "m.bin",
            local_names: &["model.onnx", "model_int8.onnx"],
        }]));
        (
            ModelSpec {
                files,
                hf_repo: "x",
                ms_repo: "x",
            },
            sha.to_string(),
        )
    }

    /// A one-file HTTP server that honours `Range` (or ignores it when told).
    fn serve(body: &'static [u8], honour_range: bool, drop_after: Option<usize>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range_from = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("range: bytes=") {
                        range_from = v.trim().trim_end_matches('-').parse().unwrap_or(0);
                    }
                }
                let (status, from) = if honour_range && range_from > 0 {
                    ("206 Partial Content", range_from)
                } else {
                    ("200 OK", 0)
                };
                let chunk = &body[from..];
                let send = drop_after.map_or(chunk.len(), |n| n.min(chunk.len()));
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    chunk.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&chunk[..send]);
            }
        });
        format!("http://127.0.0.1:{port}/m.bin")
    }

    fn fetch_to(url: &str, dir: &Path, spec: &FileSpec) -> Result<(), String> {
        let cancel = AtomicBool::new(false);
        fetch(
            url,
            &dir.join("model.onnx.part"),
            spec,
            &cancel,
            &mut |_| {},
        )
        .map_err(|e| match e {
            FetchError::Cancelled => "cancelled".into(),
            FetchError::Network(m) => format!("network: {m}"),
            FetchError::Mismatch(m) => format!("mismatch: {m}"),
        })
    }

    static BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz-model-bytes-0123456789";

    #[test]
    fn download_verifies_and_resumes() {
        let (spec, _) = tiny(BODY);
        let dir = tempfile::tempdir().unwrap();
        // the connection drops after 20 bytes; the next try asks for the rest
        let url = serve(BODY, true, Some(20));
        assert!(fetch_to(&url, dir.path(), &spec.files[0])
            .unwrap_err()
            .starts_with("network"));
        assert_eq!(
            std::fs::metadata(dir.path().join("model.onnx.part"))
                .unwrap()
                .len(),
            20
        );
        let url = serve(BODY, true, None);
        fetch_to(&url, dir.path(), &spec.files[0]).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("model.onnx.part")).unwrap(),
            BODY
        );
    }

    #[test]
    fn server_without_range_starts_over() {
        let (spec, _) = tiny(BODY);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("model.onnx.part"), &BODY[..10]).unwrap();
        let url = serve(BODY, false, None);
        fetch_to(&url, dir.path(), &spec.files[0]).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("model.onnx.part")).unwrap(),
            BODY
        );
    }

    #[test]
    fn wrong_bytes_are_a_mismatch() {
        let (spec, _) = tiny(BODY);
        let dir = tempfile::tempdir().unwrap();
        static OTHER: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz-MODEL-bytes-0123456789";
        let url = serve(OTHER, true, None);
        assert!(fetch_to(&url, dir.path(), &spec.files[0])
            .unwrap_err()
            .starts_with("mismatch"));
    }

    #[test]
    fn check_missing_ready_and_corrupt() {
        let (spec, _) = tiny(BODY);
        let tmp = tempfile::tempdir().unwrap();
        let data = DataPaths::new(tmp.path());
        assert_eq!(check(&data, &spec), ModelStatus::Missing);
        std::fs::create_dir_all(dir(&data)).unwrap();
        let file = dir(&data).join("model.onnx");
        std::fs::write(&file, BODY).unwrap();
        assert_eq!(check(&data, &spec), ModelStatus::Ready);
        assert_eq!(check(&data, &spec), ModelStatus::Ready); // from the note this time
                                                             // same size, one byte different: caught by the hash
        let mut bad = BODY.to_vec();
        bad[5] ^= 1;
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, &bad).unwrap();
        assert!(matches!(check(&data, &spec), ModelStatus::Corrupt { .. }));
        // truncated
        std::fs::write(&file, &BODY[..10]).unwrap();
        assert!(matches!(check(&data, &spec), ModelStatus::Corrupt { .. }));
    }

    #[test]
    fn folder_import_accepts_only_the_exact_file() {
        let (spec, _) = tiny(BODY);
        let tmp = tempfile::tempdir().unwrap();
        let data = DataPaths::new(tmp.path().join("kb"));
        let src = tmp.path().join("pick");
        std::fs::create_dir_all(&src).unwrap();
        assert!(import_folder(&data, &spec, &src)
            .unwrap_err()
            .contains("not found"));
        let mut bad = BODY.to_vec();
        bad[0] ^= 1;
        std::fs::write(src.join("model_int8.onnx"), &bad).unwrap();
        assert!(import_folder(&data, &spec, &src)
            .unwrap_err()
            .contains("SHA-256"));
        assert!(!dir(&data).join("model.onnx").exists());
        std::fs::write(src.join("model_int8.onnx"), BODY).unwrap();
        import_folder(&data, &spec, &src).unwrap();
        assert_eq!(check(&data, &spec), ModelStatus::Ready);
    }

    #[test]
    fn urls_name_the_expected_repository() {
        let f = &SPEC.files[0];
        assert!(Source::Huggingface
            .url(&SPEC, f)
            .starts_with("https://huggingface.co/onnx-community/bge-m3-ONNX/resolve/main/"));
        assert!(Source::Modelscope
            .url(&SPEC, f)
            .contains("modelscope.cn/api/v1/models/onnx-community/bge-m3-ONNX/repo"));
    }
}
