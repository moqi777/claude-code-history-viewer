//! Measurement spike for the P6 design: embeds chunks with an ONNX model through `ort` and answers
//! brute-force nearest-chunk queries. Not product code.
//!
//!   spike embed  <model_dir> <chunks.jsonl> <out.bin> [--max-len N] [--threads T] [--batch B] [--budget-secs S]
//!   spike query  <model_dir> <vectors.bin> <queries.json> <out.json> [--threads T] [--top K]
//!
//! chunks.jsonl: one {"id": n, "text": "..."} per line. Vectors are stored as f32, `dim` from the model.
use anyhow::{Context, Result};
use ort::{
    session::{builder::GraphOptimizationLevel, Session},
    value::Tensor,
};
use std::{io::{BufRead, Write}, path::Path, time::Instant};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

fn cur_rss_mb() -> f64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok();
    out.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
}

fn maxrss_mb() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    ru.ru_maxrss as f64 / (1024.0 * 1024.0) // bytes on macOS
}

struct Model {
    session: Session,
    tok: Tokenizer,
    mean_pool: bool,
    wants_type_ids: bool,
    query_prefix: String,
    passage_prefix: String,
}

fn flag(args: &[String], name: &str, default: usize) -> usize {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn load(dir: &Path, threads: usize, max_len: usize) -> Result<Model> {
    let t0 = Instant::now();
    let mut tok = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow::anyhow!("{e}"))?;
    tok.with_truncation(Some(TruncationParams { max_length: max_len, ..Default::default() })).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pad_id = tok.get_padding().map(|p| p.pad_id).unwrap_or(0);
    let _ = pad_id;
    let t_tok = t0.elapsed();
    eprintln!("after tokenizer: rss {:.0} MB", maxrss_mb());
    if std::env::var("SPIKE_TOK_ONLY").is_ok() { std::process::exit(0); }
    let opt = std::env::var("SPIKE_OPT").ok().and_then(|v| v.parse::<u8>().ok()).unwrap_or(3);
    let level = match opt { 0 => GraphOptimizationLevel::Disable, 1 => GraphOptimizationLevel::Level1, 2 => GraphOptimizationLevel::Level2, _ => GraphOptimizationLevel::Level3 };
    let arena = std::env::var("SPIKE_NO_ARENA").is_err();
    let mut b = Session::builder()?
        .with_optimization_level(level)?
        .with_intra_threads(threads)?
        .with_memory_pattern(std::env::var("SPIKE_NO_PATTERN").is_err())?;
    if std::env::var("SPIKE_NO_PREPACK").is_ok() { b = b.with_config_entry("session.disable_prepacking", "1")?; }
    b = b.with_execution_providers([ort::execution_providers::CPUExecutionProvider::default().with_arena_allocator(arena).build()])?;
    let session = b.commit_from_file(dir.join("model.onnx"))?;
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    let wants_type_ids = session.inputs.iter().any(|i| i.name == "token_type_ids");
    let (mean_pool, qp, pp) = if name.contains("e5") {
        (true, "query: ", "passage: ")
    } else if name.contains("small-zh") {
        (false, "为这个句子生成表示以用于检索相关文章：", "")
    } else {
        (false, "", "")
    };
    eprintln!("loaded {name}: tokenizer {:?}, total {:?}, inputs {:?}, peak rss {:.0} MB, current rss {:.0} MB", t_tok, t0.elapsed(),
        session.inputs.iter().map(|i| i.name.clone()).collect::<Vec<_>>(), maxrss_mb(), cur_rss_mb());
    Ok(Model { session, tok, mean_pool, wants_type_ids, query_prefix: qp.into(), passage_prefix: pp.into() })
}

impl Model {
    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut tok = self.tok.clone();
        tok.with_padding(Some(PaddingParams { strategy: PaddingStrategy::BatchLongest, ..Default::default() }));
        let enc = tok.encode_batch(texts.to_vec(), true).map_err(|e| anyhow::anyhow!("{e}"))?;
        let b = enc.len();
        let s = enc[0].get_ids().len();
        let mut ids = Vec::with_capacity(b * s);
        let mut mask = Vec::with_capacity(b * s);
        for e in &enc {
            ids.extend(e.get_ids().iter().map(|&x| x as i64));
            mask.extend(e.get_attention_mask().iter().map(|&x| x as i64));
        }
        let shape = [b as i64, s as i64];
        let mut inputs = ort::inputs![
            "input_ids" => Tensor::from_array((shape, ids))?,
            "attention_mask" => Tensor::from_array((shape, mask.clone()))?
        ];
        if self.wants_type_ids {
            inputs.push(("token_type_ids".into(), Tensor::from_array((shape, vec![0i64; b * s]))?.into()));
        }
        let outputs = self.session.run(inputs)?;
        let (shp, data) = outputs[0].try_extract_tensor::<f32>()?;
        let h = shp[2] as usize;
        let mut out = Vec::with_capacity(b);
        for i in 0..b {
            let mut v = vec![0f32; h];
            if self.mean_pool {
                let mut n = 0f32;
                for t in 0..s {
                    if mask[i * s + t] == 1 {
                        n += 1.0;
                        for d in 0..h { v[d] += data[(i * s + t) * h + d]; }
                    }
                }
                for x in &mut v { *x /= n.max(1.0); }
            } else {
                v.copy_from_slice(&data[(i * s) * h..(i * s) * h + h]);
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for x in &mut v { *x /= norm; }
            out.push(v);
        }
        Ok(out)
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let threads = flag(&args, "--threads", 4);
    let max_len = flag(&args, "--max-len", 256);
    match args[1].as_str() {
        "embed" => {
            let dir = Path::new(&args[2]);
            let mut m = load(dir, threads, max_len)?;
            let file = std::io::BufReader::new(std::fs::File::open(&args[3])?);
            let mut rows: Vec<(u64, String)> = Vec::new();
            for line in file.lines() {
                let v: serde_json::Value = serde_json::from_str(&line?)?;
                rows.push((v["id"].as_u64().unwrap(), format!("{}{}", m.passage_prefix, v["text"].as_str().unwrap())));
            }
            let budget = flag(&args, "--budget-secs", 0);
            let batch = flag(&args, "--batch", 16);
            // sort by length so batches pad little
            let mut order: Vec<usize> = (0..rows.len()).collect();
            order.sort_by_key(|&i| rows[i].1.chars().count());
            let t0 = Instant::now();
            let mut out_ids: Vec<u64> = Vec::new();
            let mut out_vecs: Vec<f32> = Vec::new();
            let mut dim = 0;
            for chunk in order.chunks(batch) {
                let texts: Vec<String> = chunk.iter().map(|&i| rows[i].1.clone()).collect();
                let vs = m.embed(&texts)?;
                for (k, v) in vs.into_iter().enumerate() {
                    dim = v.len();
                    out_ids.push(rows[chunk[k]].0);
                    out_vecs.extend(v);
                }
                if budget > 0 && t0.elapsed().as_secs() >= budget as u64 { break; }
                if out_ids.len() % 2000 < batch { eprintln!("  {} / {} in {:?}, rss {:.0} MB", out_ids.len(), rows.len(), t0.elapsed(), maxrss_mb()); }
            }
            let el = t0.elapsed();
            let mut f = std::io::BufWriter::new(std::fs::File::create(&args[4])?);
            f.write_all(&(out_ids.len() as u64).to_le_bytes())?;
            f.write_all(&(dim as u64).to_le_bytes())?;
            for id in &out_ids { f.write_all(&id.to_le_bytes())?; }
            for x in &out_vecs { f.write_all(&x.to_le_bytes())?; }
            println!("{{\"embedded\":{},\"of\":{},\"dim\":{},\"secs\":{:.1},\"per_sec\":{:.1},\"peak_rss_mb\":{:.0},\"end_rss_mb\":{:.0}}}",
                out_ids.len(), rows.len(), dim, el.as_secs_f64(), out_ids.len() as f64 / el.as_secs_f64(), maxrss_mb(), cur_rss_mb());
        }
        "query" => {
            let t_start = Instant::now();
            let dir = Path::new(&args[2]);
            let mut m = load(dir, threads, max_len)?;
            let t_loaded = t_start.elapsed();
            let raw = std::fs::read(&args[3])?;
            let n = u64::from_le_bytes(raw[0..8].try_into()?) as usize;
            let dim = u64::from_le_bytes(raw[8..16].try_into()?) as usize;
            let ids: Vec<u64> = (0..n).map(|i| u64::from_le_bytes(raw[16 + i * 8..24 + i * 8].try_into().unwrap())).collect();
            let base = 16 + n * 8;
            let vecs: Vec<f32> = (0..n * dim).map(|i| f32::from_le_bytes(raw[base + i * 4..base + i * 4 + 4].try_into().unwrap())).collect();
            let mut vecs = vecs;
            if std::env::var("SPIKE_Q8").is_ok() {
                for r in 0..n {
                    let row = &mut vecs[r * dim..(r + 1) * dim];
                    let m = row.iter().fold(0f32, |a, b| a.max(b.abs())).max(1e-9);
                    for x in row.iter_mut() { *x = ((*x / m) * 127.0).round() / 127.0 * m; }
                }
            }
            let t_vecs = t_start.elapsed();
            let queries: Vec<String> = serde_json::from_str(&std::fs::read_to_string(&args[4])?)?;
            let top = flag(&args, "--top", 60);
            let mut result = Vec::new();
            let mut times = Vec::new();
            for q in &queries {
                let t = Instant::now();
                let qv = m.embed(&[format!("{}{}", m.query_prefix, q)])?.remove(0);
                let t_embed = t.elapsed();
                let mut scored: Vec<(f32, u64)> = (0..n).map(|i| (vecs[i * dim..(i + 1) * dim].iter().zip(&qv).map(|(a, b)| a * b).sum::<f32>(), ids[i])).collect();
                scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                times.push((t_embed.as_secs_f64() * 1000.0, t.elapsed().as_secs_f64() * 1000.0));
                result.push(serde_json::json!({"q": q, "top": scored.iter().take(top).map(|(s, i)| serde_json::json!([i, s])).collect::<Vec<_>>()}));
            }
            std::fs::write(&args[5], serde_json::to_string(&result)?)?;
            println!("{{\"load_model_ms\":{:.0},\"load_vectors_ms\":{:.0},\"first_query_embed_ms\":{:.0},\"first_query_total_ms\":{:.0},\"median_embed_ms\":{:.0},\"median_total_ms\":{:.0},\"peak_rss_mb\":{:.0}}}",
                t_loaded.as_secs_f64() * 1000.0, (t_vecs - t_loaded).as_secs_f64() * 1000.0, times[0].0, times[0].1,
                { let mut v: Vec<f64> = times.iter().map(|t| t.0).collect(); v.sort_by(|a, b| a.total_cmp(b)); v[v.len() / 2] },
                { let mut v: Vec<f64> = times.iter().map(|t| t.1).collect(); v.sort_by(|a, b| a.total_cmp(b)); v[v.len() / 2] },
                maxrss_mb());
        }
        _ => anyhow::bail!("unknown command"),
    }
    Ok(())
}
