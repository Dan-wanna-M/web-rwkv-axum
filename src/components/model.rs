use anyhow::{Error, Result};
use futures::executor::block_on;
use itertools::Itertools;
use web_rwkv::{
    context::Context,
    model::{
        v4::{self},
        v5, ModelInfo, ModelState, StateBuilder, ModelBase, run::ModelRun, softmax::ModelSoftmax,
    },
};

#[derive(Debug, Clone)]
pub enum AxumModelState {
    V4(v4::ModelState),
    V5(v5::ModelState),
}

#[derive(Debug, Clone)]
pub enum AxumBackedState {
    V4(v4::BackedState),
    V5(v5::BackedState),
}

impl AxumModelState {
    pub fn new(context: &Context, model: &AxumModel, batch_size: usize) -> Self {
        match model {
            AxumModel::V4(model) => Self::V4(
                StateBuilder::new(context, model.info())
                    .with_max_batch(batch_size)
                    .build(),
            ),
            AxumModel::V5(model) => Self::V5(
                StateBuilder::new(context, model.info())
                    .with_max_batch(batch_size)
                    .build(),
            ),
        }
    }

    pub fn new_sized(
        context: &Context,
        model: &AxumModel,
        batch_size: usize,
        chunk_size: Option<usize>,
    ) -> Self {
        match model {
            AxumModel::V4(model) => Self::V4(
                StateBuilder::new(context, model.info())
                    .with_chunk_size(chunk_size.unwrap_or(model.info().num_layer))
                    .with_max_batch(batch_size)
                    .build(),
            ),
            AxumModel::V5(model) => Self::V5(
                StateBuilder::new(context, model.info())
                    .with_chunk_size(chunk_size.unwrap_or(model.info().num_layer))
                    .with_max_batch(batch_size)
                    .build(),
            ),
        }
    }

    pub fn blit_batch(
        &self,
        dst: &AxumModelState,
        src_index: usize,
        dst_index: usize,
    ) -> Result<()> {
        match (self, dst) {
            (AxumModelState::V4(state), AxumModelState::V4(dst)) => {
                Ok(state.blit_batch(dst, src_index, dst_index)?)
            }
            (AxumModelState::V5(state), AxumModelState::V5(dst)) => {
                Ok(state.blit_batch(dst, src_index, dst_index)?)
            }
            _ => Err(Error::msg("Mismatched state type!")),
        }
    }
}

impl AxumBackedState {
    pub fn new(context: &Context, model: &AxumModel, chunk_size: Option<usize>) -> Self {
        match model {
            AxumModel::V4(model) => Self::V4(
                StateBuilder::new(context, model.info())
                    .with_chunk_size(chunk_size.unwrap_or(model.info().num_layer))
                    .build_backed(),
            ),
            AxumModel::V5(model) => Self::V5(
                StateBuilder::new(context, model.info())
                    .with_chunk_size(chunk_size.unwrap_or(model.info().num_layer))
                    .build_backed(),
            ),
        }
    }

    pub fn load_to(&self, dst: &AxumModelState, dst_index: usize) -> Result<()> {
        match (self, dst) {
            (AxumBackedState::V4(state), AxumModelState::V4(model_state)) => {
                model_state.load_batch(state, dst_index)
            }
            (AxumBackedState::V5(state), AxumModelState::V5(model_state)) => {
                model_state.load_batch(state, dst_index)
            }
            _ => Err(Error::msg("Mismatched state type!")),
        }
    }

    pub fn back_from(dst: &AxumModelState, dst_index: usize) -> Result<AxumBackedState> {
        match dst {
            AxumModelState::V4(dst) => Ok(AxumBackedState::V4(block_on(dst.back_batch(dst_index))?)),
            AxumModelState::V5(dst) => Ok(AxumBackedState::V5(block_on(dst.back_batch(dst_index))?)),
        }
    }
}

pub enum AxumModel {
    V4(v4::Model<'static>),
    V5(v5::Model<'static>),
}

impl AxumModel {
    pub fn run(
        &self,
        tokens: &mut Vec<Vec<u16>>,
        state: &AxumModelState,
    ) -> Result<Vec<Option<Vec<f32>>>> {
        match &self {
            Self::V4(model) => {
                if let AxumModelState::V4(state) = state {
                    block_on(model.run(tokens, state))
                } else {
                    Err(Error::msg("Mismatched state type!"))
                }
            }
            Self::V5(model) => {
                if let AxumModelState::V5(state) = state {
                    block_on(model.run(tokens, state))
                } else {
                    Err(Error::msg("Mismatched state type!"))
                }
            }
        }
    }

    pub fn info(&self) -> &ModelInfo {
        match self {
            AxumModel::V4(model) => model.info(),
            AxumModel::V5(model) => model.info(),
        }
    }

    pub fn softmax(&self, input: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>> {
        let input = input.into_iter().map(|x| Some(x)).collect_vec();
        Ok(match self {
            AxumModel::V4(model) => block_on(model.softmax(input)),
            AxumModel::V5(model) => block_on(model.softmax(input)),
        }?
        .into_iter()
        .map(|x| x.unwrap())
        .collect())
    }

    pub fn infer(
        &self,
        tokens: &mut Vec<Vec<u16>>,
        state: &AxumModelState,
    ) -> Result<Vec<Option<Vec<f32>>>> {
        loop {
            let logits =self.run(tokens, state)?;
            if logits.iter().any(|l| l.is_some()) {
                break Ok(logits);
            }
        }
    }
}
