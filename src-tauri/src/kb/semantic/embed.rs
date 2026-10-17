//! Turning text into vectors with ONNX Runtime.
//!
//! [`Embedder`] is the seam: the real one ([`OrtEmbedder`]) needs the model
//! files, tests use a fake. Both produce unit-length vectors of [`DIM`] numbers.

use std::path::Path;

use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::{DIM, MAX_TOKENS};

pub trait Embedder {
    /// Vectors for texts that will be searched (stored side).
    fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
    /// The vector of a search query. (bge-m3 uses no prefix; kept separate so
    /// another model can.)
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, String> {
        self.embed_passages(&[text.to_string()])?
            .pop()
            .ok_or_else(|| "no vector returned".to_string())
    }
}

pub struct OrtEmbedder {
    session: Session,
    tokenizer: Tokenizer,
}

fn e(what: &str, err: impl std::fmt::Display) -> String {
    format!("{what}: {err}")
}

impl OrtEmbedder {
    pub fn load(dir: &Path, threads: usize) -> Result<Self, String> {
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|x| e("cannot read the tokenizer", x))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|x| e("tokenizer", x))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        let session = Session::builder()
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|b| b.with_intra_threads(threads.max(1)))
            .and_then(|b| b.commit_from_file(dir.join("model.onnx")))
            .map_err(|x| e("cannot load the model", x))?;
        Ok(Self { session, tokenizer })
    }
}

impl Embedder for OrtEmbedder {
    #[allow(clippy::cast_possible_wrap)]
    fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let enc = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|x| e("tokenizer", x))?;
        let b = enc.len();
        let s = enc[0].get_ids().len();
        let mut ids: Vec<i64> = Vec::with_capacity(b * s);
        let mut mask: Vec<i64> = Vec::with_capacity(b * s);
        for item in &enc {
            ids.extend(item.get_ids().iter().map(|&x| i64::from(x)));
            mask.extend(item.get_attention_mask().iter().map(|&x| i64::from(x)));
        }
        let shape = [b as i64, s as i64];
        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array((shape, ids)).map_err(|x| e("input", x))?,
            "attention_mask" => Tensor::from_array((shape, mask)).map_err(|x| e("input", x))?
        ];
        let outputs = self.session.run(inputs).map_err(|x| e("inference", x))?;
        let (dims, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|x| e("output", x))?;
        if dims.len() != 3 || dims[2] as usize != DIM || data.len() != b * s * DIM {
            return Err(format!("unexpected model output shape {dims:?}"));
        }
        // First position (the [CLS]-style token), normalised.
        Ok((0..b)
            .map(|i| normalise(&data[i * s * DIM..i * s * DIM + DIM]))
            .collect())
    }
}

pub fn normalise(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter().map(|x| x / norm).collect()
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use crate::kb::semantic::chunk::text_sha;

    /// Deterministic stand-in: words hash to coordinates, so texts sharing
    /// words are close. Counts calls so tests can see what was recomputed.
    #[derive(Default)]
    pub struct FakeEmbedder {
        pub texts_embedded: usize,
        pub fail: bool,
    }

    pub fn vector_of(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; DIM];
        for w in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let h = text_sha(&w.to_lowercase());
            let i = usize::from_str_radix(&h[..6], 16).unwrap_or(0) % DIM;
            v[i] += 1.0;
        }
        if v.iter().all(|x| *x == 0.0) {
            v[0] = 1.0;
        }
        normalise(&v)
    }

    impl Embedder for FakeEmbedder {
        fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            if self.fail {
                return Err("fake failure".into());
            }
            self.texts_embedded += texts.len();
            Ok(texts.iter().map(|t| vector_of(t)).collect())
        }
    }
}
