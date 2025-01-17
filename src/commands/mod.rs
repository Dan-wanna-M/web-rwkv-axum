use anyhow::{Error, Ok, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::{app::AppState, register_handlers};

mod handle_infer;
mod handle_samplers;
mod handle_states;
mod handle_transformers;
mod helpers;

pub mod types;

#[derive(Debug, Deserialize)]
pub struct TextCommand {
    pub echo_id: String,
    command: String,
    data: Option<Value>,
}

impl TextCommand {
    pub async fn handle(&self, state: AppState) -> Result<Value> {
        register_handlers!(
            self,
            state,
            [
                // States
                handle_states::create_state,
                handle_states::copy_state,
                handle_states::update_state,
                handle_states::delete_state,
                //Transformers
                handle_transformers::create_transformer,
                handle_transformers::copy_transformer,
                handle_transformers::update_transformer,
                handle_transformers::delete_transformer,
                handle_transformers::reset_transformer,
                //Samplers
                handle_samplers::create_sampler,
                handle_samplers::copy_sampler,
                handle_samplers::update_sampler,
                handle_samplers::delete_sampler,
                handle_samplers::reset_sampler,
                //Infer
                handle_infer::infer,
            ]
        )
    }
}
