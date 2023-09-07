use std::sync::Arc;

use anyhow::{Error, Ok, Result};
use dashmap::DashMap;
use tokio::sync::{mpsc::Sender, oneshot};
use web_rwkv::{context::Context, model::Model, tokenizer::Tokenizer};

use crate::{
    config::ModelConfig,
    helper::{Logits, State},
    states::{
        infer::{InferContext, InferRequest, InferResult},
        sampler::Samplers,
        softmax::Softmax,
        transformer::Transformers,
    },
};

/// Global state holder of the entire app.
pub struct AppState {
    pub config: ModelConfig,
    pub samplers: Arc<Samplers>,
    pub transformers: Arc<Transformers>,
    infer_queue: Sender<Vec<InferRequest>>,
    softmax_queue: Sender<Vec<(Vec<f32>, oneshot::Sender<Vec<f32>>)>>,
    // State holders
    // Can be None to represent state not created by pipeline yet
    infer_states: Arc<DashMap<String, Option<State>>>,
    pub tokenizer: Arc<Tokenizer>,
    pub context: Context,
    pub model: Arc<Model<'static>>,
}

impl AppState {
    pub async fn new(
        config: &ModelConfig,
        infer_queue: Sender<Vec<InferRequest>>,
        softmax_queue: Sender<Vec<(Vec<f32>, oneshot::Sender<Vec<f32>>)>>,
        context: Context,
        model: Arc<Model<'static>>,
    ) -> Result<Self> {
        Ok(AppState {
            config: config.clone(),
            samplers: Arc::new(Samplers::new()),
            transformers: Arc::new(Transformers::new()),
            infer_queue,
            softmax_queue,
            infer_states: Arc::new(DashMap::with_capacity(128)),
            tokenizer: Arc::new(config.tokenizer.load_tokenizer().await?),
            context,
            model,
        })
    }

    pub async fn update_state(&self, id: Vec<String>, tokens: Vec<Vec<u16>>) -> Result<()> {
        let _ = self.infer(id, tokens).await?;
        Ok(())
    }

    pub async fn create_state(&self, id: String) -> Result<()> {
        if self.infer_states.contains_key(&id) {
            return Err(Error::msg("State already exists!"));
        }
        self.infer_states.insert(id, None);
        Ok(())
    }

    #[inline(always)]
    pub fn has_state(&self, id: &String) -> bool {
        self.infer_states.contains_key(id)
    }

    pub async fn copy_state(&self, src: String, dst: String) -> Result<()> {
        if self.infer_states.contains_key(&dst) {
            return Err(Error::msg("Destination state id already exists!"));
        }
        let src = self
            .infer_states
            .get(&src)
            .ok_or(Error::msg("State doesn't exist!"))?
            .clone();
        self.infer_states.insert(dst, src);
        Ok(())
    }

    pub async fn delete_state(&self, id: String) -> Result<()> {
        self.infer_states
            .remove(&id)
            .ok_or(Error::msg("State doesn't exist!"))
            .map(|_| ())
    }

    pub fn tokenize(&self, input: &Vec<u8>) -> Result<Vec<u16>> {
        Ok(self.tokenizer.encode(&input)?)
    }

    pub async fn infer(
        &self,
        state_keys: Vec<String>,
        token_vecs: Vec<Vec<u16>>,
    ) -> Result<Vec<Logits>> {
        let states = state_keys
            .iter()
            .map(|key| {
                self.infer_states
                    .get(key)
                    .ok_or(Error::msg(format!("State {} doesn't exist!", key)))
            })
            .collect::<Result<Vec<_>>>()?;

        let requests = states
            .into_iter()
            .zip(token_vecs.into_iter())
            .map(|(state, tokens)| InferContext {
                state: state.clone(),
                tokens,
            })
            .collect();

        let results = InferRequest::send(requests, self.infer_queue.clone()).await?;

        Ok(results
            .into_iter()
            .zip(state_keys.into_iter())
            .map(|(InferResult { state, logits }, state_key)| {
                self.infer_states.insert(state_key, Some(state));
                logits
            })
            .collect())
    }

    pub async fn softmax(&self, logits: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>> {
        Softmax::softmax(logits, self.softmax_queue.clone()).await
    }
}

pub type SharedState = Arc<AppState>;