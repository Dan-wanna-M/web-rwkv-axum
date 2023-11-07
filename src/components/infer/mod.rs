use std::collections::HashSet;

use anyhow::{Error, Result};
use dashmap::mapref::one::RefMut;
use rayon::prelude::*;

use crate::app::AppState;

use self::reset::ResetSetting;

use super::normalizer::types::Normalizer;
use super::sampler::types::Sampler;
use super::terminal::types::Terminal;
use super::transformer::types::Transformer;
use super::InferenceInterruption;

pub mod reset;
pub mod state;
pub mod tokens;

/// Wrapped logic for updating, transforming-sampling, and terminating.
pub struct SamplePipeline<'a> {
    transformers: Vec<Vec<RefMut<'a, String, Box<dyn Transformer>>>>,
    sampler: RefMut<'a, String, Box<dyn Sampler>>,
    terminal: RefMut<'a, String, Box<dyn Terminal>>,
    normalizer: Option<RefMut<'a, String, Box<dyn Normalizer>>>,
    reset_setting: ResetSetting,
}

impl<'a> SamplePipeline<'a> {
    pub fn new(
        state: &'a AppState,
        transformers: &Vec<Vec<String>>,
        sampler: &str,
        terminal: &str,
        normalizer: &Option<String>,
        reset_setting: ResetSetting,
    ) -> Result<SamplePipeline<'a>> {
        let mut uniq = HashSet::new();
        if !transformers
            .iter()
            .flatten()
            .all(move |x| uniq.insert(x.clone()))
        {
            return Err(Error::msg("All transformer ids must be unique!"));
        };

        let transformers = transformers
            .into_iter()
            .map(|x| {
                x.into_iter()
                    .map(|x| {
                        state
                            .0
                            .transformers
                            .get_transformer(&x)
                            .ok_or(Error::msg("Transformer not found!"))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;

        let sampler = state
            .0
            .samplers
            .get_sampler(sampler)
            .ok_or(Error::msg("Sampler not found!"))?;

        let terminal = state
            .0
            .terminals
            .get_terminal(terminal)
            .ok_or(Error::msg("Terminal not found!"))?;

        let normalizer = if let Some(normalizer) = normalizer.as_ref() {
            Some(
                state
                    .0
                    .normalizers
                    .get_normalizer(&normalizer)
                    .ok_or(Error::msg("Normalizer not found!"))?,
            )
        } else {
            None
        };

        Ok(Self {
            transformers,
            sampler,
            terminal,
            normalizer,
            reset_setting,
        })
    }

    /// Should call this in blocking as it can be computation heavy
    pub fn update(&mut self, tokens: &Vec<Vec<u16>>) -> Result<(), InferenceInterruption> {
        self.sampler.update(tokens)?;
        if self.transformers.len() != tokens.len() {
            return Err(InferenceInterruption::Error(Error::msg(
                "Transformer/tokens batch size mismatch!",
            )));
        }
        self.transformers
            .par_iter_mut()
            .zip(tokens.par_iter())
            .map(|(transformers, tokens)| {
                for transformer in transformers {
                    transformer.update(tokens)?;
                }
                Ok(())
            })
            .collect::<Result<Vec<_>, InferenceInterruption>>()?;
        Ok(())
    }

    pub fn reset(&mut self) {
        let ResetSetting {
            transformers,
            sampler,
            normalizer,
        } = &self.reset_setting;

        self.transformers
            .iter_mut()
            .flatten()
            .zip(transformers)
            .for_each(|(transformer, &flag)| {
                if flag {
                    transformer.clear();
                }
            });

        if *sampler {
            self.sampler.clear();
        }

        if *normalizer {
            if let Some(normalizer) = self.normalizer.as_mut() {
                normalizer.clear();
            }
        }
    }

    /// Should call this in blocking as it can be computation heavy
    pub fn sample(&self, logits: Vec<Vec<f32>>, app_state: &AppState) -> u16 {
        let logits = self
            .transformers
            .par_iter()
            .zip(logits.into_par_iter())
            .map(|(transformers, mut logits)| {
                for transformer in transformers {
                    logits = transformer.transform(logits);
                }
                logits
            })
            .collect::<Vec<_>>();
        let logits = if let Some(normalizer) = &self.normalizer {
            normalizer.normalize(logits)
        } else {
            app_state.softmax_blocking(logits)
        };
        self.sampler.sample(logits)
    }

    #[inline(always)]
    pub fn terminate(&mut self, result: &Vec<u16>, token_count: usize) -> Result<bool> {
        self.terminal.terminate(result, token_count)
    }
}