use anyhow::{Error, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{app::AppState, commands::helpers, states::InferenceInterruption};

#[derive(Debug, Deserialize)]
struct InferPayload {
    tokens: Vec<Value>,
    states: Vec<String>,
    transformers: Vec<Vec<String>>,
    sampler: String,
    update_prompt: bool,
    reset_on_exhaustion: bool,
}

fn transform_logits(
    app_state: AppState,
    mut logits: Vec<f32>,
    transformers: &Vec<String>,
) -> Result<Vec<f32>> {
    for transformer in transformers {
        logits = app_state
            .0
            .transformers
            .transform_logits(transformer, logits)?
    }
    Ok(logits)
}

async fn infer_and_sample(
    app_state: AppState,
    state_ids: &Vec<String>,
    transformers: &Vec<Vec<String>>,
    tokens: Vec<Vec<u16>>,
    sampler: &String,
    update_prompts: bool,
    reset_on_exhaustion: bool,
) -> Result<u16, InferenceInterruption> {
    if update_prompts {
        tokio::task::block_in_place(|| -> Result<(), InferenceInterruption> {
            // This is the last place anything can stop the infer, if you want
            // to stop the infer in case of additional termination from
            // transformer/sampler, you must do it from updates, or the state
            // will be polluted by the token input.

            // Transformer and sampler should be aware of the exhaustion, where
            // it should know it will fail no matter what logits/probs are
            // given at sample/transformation time. and if it knows, it must
            // throw an error.
            let transformer_update = transformers
                .par_iter()
                .zip(tokens.par_iter())
                .map(|(t_ids, tokens)| {
                    for t_id in t_ids {
                        let result = app_state.0.transformers.update_transformer(t_id, tokens);
                        if let Err(InferenceInterruption::Exhaustion) = result {
                            if reset_on_exhaustion {
                                app_state.0.transformers.reset_transformer(t_id).unwrap();
                            }
                        }
                        result?
                    }
                    Ok(())
                })
                .collect::<Result<Vec<()>, InferenceInterruption>>();
            let sampler_update = app_state.0.samplers.update_sampler(&sampler, &tokens);
            if let Err(InferenceInterruption::Exhaustion) = sampler_update {
                if reset_on_exhaustion {
                    app_state.0.samplers.reset_sampler(&sampler).unwrap();
                }
            }
            transformer_update.and(sampler_update)
        })?;
    }

    let logits = app_state
        .infer(state_ids.clone(), tokens)
        .await
        .map_err(|e| InferenceInterruption::Error(e))?;

    // In case if transformation is needed, we block the current thread and use rayon to
    // transform each logits
    let logits = if transformers.iter().any(|x| !x.is_empty()) {
        tokio::task::block_in_place(|| {
            logits
                .into_par_iter()
                .map(|x| x.0)
                .zip(transformers.par_iter())
                .map(|(logits, t_ids)| transform_logits(app_state.clone(), logits, t_ids))
                .collect::<Result<Vec<_>>>()
        })
        .map_err(|e| InferenceInterruption::Error(e))?
    } else {
        logits.into_iter().map(|x| x.0).collect()
    };
    let probs = app_state.softmax(logits).await;
    return tokio::task::block_in_place(move || app_state.0.samplers.sample_token(&sampler, probs))
        .map_err(|e| InferenceInterruption::Error(e));
}

#[derive(Debug, Serialize)]
struct InferResponse {
    value: String,
    last_token: u16,
    inferred_tokens: usize,
}

pub async fn infer(data: Option<Value>, state: AppState) -> Result<Value> {
    if let Some(data) = data {
        let InferPayload {
            tokens,
            states,
            transformers,
            sampler,
            update_prompt,
            reset_on_exhaustion,
        } = serde_json::from_value::<InferPayload>(data)?;

        if tokens.len() != states.len() || states.len() != transformers.len() {
            return Err(Error::msg(
                "State, token, transformer length must be matched!",
            ));
        }

        if states.iter().any(|x| !state.has_state(x)) {
            return Err(Error::msg("One or more state ids not exist!"));
        }

        if transformers
            .iter()
            .flatten()
            .any(|x| !state.0.transformers.has_transformer(x))
        {
            return Err(Error::msg("One or more transformer ids not exist!"));
        }

        if !state.0.samplers.has_sampler(&sampler) {
            return Err(Error::msg("Sampler id does not exist!"));
        }

        let tokens = tokens
            .into_iter()
            .map(|v| helpers::to_tokens(&state, v))
            .collect::<Result<Vec<_>>>()?;

        if tokens.is_empty() || tokens.iter().any(|x| x.is_empty()) {
            return Err(Error::msg("Empty token list!"));
        }

        let (result, last_token, inferred_tokens) = {
            let mut out_tokens = Vec::with_capacity(4);
            let mut inferred_tokens: usize = 1usize;
            let mut result = String::new();

            // Locks state_size slots for the infer
            let _permits = state.0.batch_request.request(states.len());

            // Feed prompt first, at least the first token should be ok
            // or there must be some problem in the infer pipeline
            out_tokens.push(
                infer_and_sample(
                    state.clone(),
                    &states,
                    &transformers,
                    tokens,
                    &sampler,
                    update_prompt,
                    false,
                )
                .await
                .map_err(|e| match e {
                    InferenceInterruption::Exhaustion => Error::msg(
                        "Sampler/transformer is exhausted at the start, inference won't continue.",
                    ),
                    InferenceInterruption::Error(e) => e,
                })?,
            );

            let mut last_token = *out_tokens.last().unwrap();

            loop {
                if let Ok(Ok(partial)) = state
                    .0
                    .tokenizer
                    .decode(&out_tokens.as_slice())
                    .map(|x| String::from_utf8(x))
                {
                    result.push_str(partial.as_str());
                    inferred_tokens += out_tokens.len();
                    out_tokens.clear()
                }

                // TODO: implement terminal here, also we need to ensure that
                // out token will be empty when output, or it will be extremely tricky
                // to hand over the out token.
                if inferred_tokens >= 10 && out_tokens.is_empty() {
                    break (result, last_token, inferred_tokens);
                }

                // Not ready, infer next one using last token
                out_tokens.push(
                    match infer_and_sample(
                        state.clone(),
                        &states,
                        &transformers,
                        vec![vec![last_token]; states.len()],
                        &sampler,
                        update_prompt,
                        reset_on_exhaustion,
                    )
                    .await
                    {
                        Ok(token) => token,
                        // Exhausted, so stop infer.
                        Err(InferenceInterruption::Exhaustion) => {
                            break (result, last_token, inferred_tokens);
                        }
                        // A sampling/transformation error occurred, inference
                        // is terminated
                        Err(InferenceInterruption::Error(error)) => Err(error)?,
                    },
                );
                last_token = *out_tokens.last().unwrap();
            }
        };

        Ok(serde_json::to_value(InferResponse {
            value: result,
            last_token,
            inferred_tokens,
        })?)
    } else {
        Err(Error::msg(
            "Field data is needed to specify infer pipeline!",
        ))
    }
}
