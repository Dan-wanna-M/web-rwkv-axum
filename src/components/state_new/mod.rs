use anyhow::{Error, Result};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use web_rwkv::context::Context;

use crate::config::ModelConfig;

use self::{
    pool::{InferPool, InferRequest},
    state::InferState,
};

use super::{model::AxumModel, permit::BatchRequest};

mod pool;
mod state;

struct InnerStates {
    context: Context,
    model: Arc<AxumModel>,
    pool: InferPool,
    states: DashMap<String, InferState>,
    request_queue: mpsc::Sender<Vec<InferRequest>>,
}

#[derive(Clone)]
pub struct InferStates(Arc<InnerStates>);

impl InferStates {
    pub async fn new(
        config: &ModelConfig,
        context: Context,
        model: Arc<AxumModel>,
        batch_lock: BatchRequest,
    ) -> Result<Self> {
        let pool = InferPool::new(
            context.clone(),
            model.clone(),
            config.model.get_max_concurrency(),
            config.model.get_batch_size(),
            batch_lock,
        );
        let sender = pool.start_loop().await;

        Ok(Self(Arc::new(InnerStates {
            context,
            model,
            pool,
            states: DashMap::with_capacity(128),
            request_queue: sender,
        })))
    }

    pub async fn infer(
        &self,
        states: &Vec<String>,
        tokens: Vec<Vec<u16>>,
    ) -> Result<Vec<Vec<f32>>> {
        let states = states
            .iter()
            .map(|state_id| self.get_state(&state_id))
            .collect::<Vec<_>>();

        let (receivers, requests): (Vec<oneshot::Receiver<Vec<f32>>>, Vec<InferRequest>) = states
            .into_iter()
            .zip(tokens.into_iter())
            .map(|(state, tokens)| {
                let (sender, receiver) = oneshot::channel();
                (receiver, InferRequest::new(state, tokens, sender))
            })
            .unzip();

        self.0.request_queue.send(requests).await?;

        Ok(futures::future::join_all(receivers)
            .await
            .into_iter()
            .map(|x| x.map_err(|_| Error::msg("Error while receiving message!")))
            .collect::<Result<_>>()?)
    }

    pub fn create_state(&self, state_id: &str) -> Result<()> {
        if self.0.states.contains_key(state_id) {
            return Err(Error::msg("State already exists!"));
        }
        self.0.states.insert(
            state_id.to_string(),
            InferState::new(
                state_id.to_string(),
                self.0.context.clone(),
                self.0.model.clone(),
            ),
        );
        Ok(())
    }

    pub fn copy_state(&self, src: &str, dst: &str) -> Result<()> {
        if self.0.states.contains_key(dst) {
            return Err(Error::msg("Destination state already exists!"));
        }
        if !self.0.states.contains_key(src) {
            return Err(Error::msg("Source state id doesn't exist!"));
        }

        tokio::task::block_in_place(|| {
            self.0.pool.sync(src);
            let dst_state = self.0.states.get(src).unwrap().clone_new(dst.to_string())?;
            self.0.states.insert(dst.to_string(), dst_state);
            Ok::<(), Error>(())
        })?;

        Ok(())
    }

    pub fn delete_state(&self, state_id: &str) -> Result<()> {
        match self.0.states.remove(state_id) {
            Some(_) => {
                self.0.states.remove(state_id);
                Ok(())
            }
            None => Err(Error::msg("State ID does not exist!")),
        }
    }

    pub fn get_state(&self, state_id: &str) -> InferState {
        self.0
            .states
            .entry(state_id.to_string())
            .or_insert_with(|| {
                InferState::new(
                    state_id.to_string(),
                    self.0.context.clone(),
                    self.0.model.clone(),
                )
            })
            .clone()
    }

    #[inline(always)]
    pub fn has_state(&self, state_id: &str) -> bool {
        self.0.states.contains_key(state_id)
    }
}