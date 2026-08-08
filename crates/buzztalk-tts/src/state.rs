//! Recurrent ONNX session state: the flow-LM attention cache and Mimi
//! decoder convolution state that get threaded through repeated calls to
//! `Session::run`.
//!
//! Ported from Block's `buzz-voice` crate (`src/pocket_april.rs`,
//! Apache-2.0) — see the crate-level attribution in `lib.rs`.

use std::borrow::Cow;

use ort::session::{SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Tensor};

use crate::bundle::{StateDtype, StateFill, StateSpec};
use crate::error::{Error, Result};

/// One live state tensor, paired with the spec that describes how to
/// (re)initialize and route it.
pub struct StateValue {
    pub spec: StateSpec,
    pub value: DynValue,
}

fn onnx_error<R>(context: impl Into<String>) -> impl FnOnce(ort::Error<R>) -> Error
where
    ort::Error<R>: Into<ort::Error>,
{
    let context = context.into();
    move |source| Error::Onnx {
        context,
        source: source.into(),
    }
}

/// Build the initial state for a graph from its manifest, following each
/// spec's `fill` policy (NaN/zeros/ones/empty).
pub fn initialize_state(specs: &[StateSpec]) -> Result<Vec<StateValue>> {
    specs
        .iter()
        .cloned()
        .map(|spec| {
            let len = shape_len(&spec.shape)?;
            let value = match spec.dtype {
                StateDtype::Float32 => {
                    let fill = match spec.fill {
                        StateFill::Nan => f32::NAN,
                        StateFill::Empty | StateFill::Zeros => 0.0,
                        StateFill::Ones => 1.0,
                    };
                    build_tensor::<f32>(&spec.shape, len, fill)?
                }
                StateDtype::Int64 => {
                    let fill = i64::from(matches!(spec.fill, StateFill::Ones));
                    build_tensor::<i64>(&spec.shape, len, fill)?
                }
                StateDtype::Bool => {
                    let fill = matches!(spec.fill, StateFill::Ones);
                    build_tensor::<bool>(&spec.shape, len, fill)?
                }
            };
            Ok(StateValue { spec, value })
        })
        .collect()
}

fn build_tensor<T>(shape: &[i64], len: usize, fill: T) -> Result<DynValue>
where
    T: ort::value::PrimitiveTensorElementType + std::fmt::Debug + Clone + 'static,
{
    if len == 0 {
        Ok(
            Tensor::<T>::new(&ort::memory::Allocator::default(), shape.to_vec())
                .map_err(onnx_error("creating an empty state tensor"))?
                .into_dyn(),
        )
    } else {
        Ok(
            Tensor::from_array((shape.to_vec(), vec![fill; len].into_boxed_slice()))
                .map_err(onnx_error("creating a state tensor"))?
                .into_dyn(),
        )
    }
}

/// Append `state`'s tensors to `inputs` under their manifest input names.
pub fn append_state_inputs<'a>(
    inputs: &mut Vec<(Cow<'a, str>, SessionInputValue<'a>)>,
    state: &'a [StateValue],
) {
    for value in state {
        inputs.push((
            Cow::Borrowed(value.spec.input_name.as_str()),
            SessionInputValue::from(&value.value),
        ));
    }
}

/// Replace each entry in `state` with the corresponding output tensor,
/// consuming `outputs`.
pub fn replace_state_from_outputs(
    state: &mut [StateValue],
    outputs: &mut SessionOutputs<'_>,
) -> Result<()> {
    for value in state {
        value.value = outputs
            .remove(value.spec.output_name.as_str())
            .ok_or_else(|| {
                Error::Synthesis(format!("missing state output `{}`", value.spec.output_name))
            })?;
    }
    Ok(())
}

fn shape_len(shape: &[i64]) -> Result<usize> {
    shape.iter().try_fold(1_usize, |len, &dim| {
        let dim = usize::try_from(dim)
            .map_err(|_| Error::Synthesis(format!("negative state dimension {dim}")))?;
        len.checked_mul(dim)
            .ok_or_else(|| Error::Synthesis(format!("state shape overflows usize: {shape:?}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_len_supports_empty_state_dimensions() {
        assert_eq!(shape_len(&[1, 128, 0]).unwrap(), 0);
        assert_eq!(shape_len(&[2, 1, 8, 1000, 64]).unwrap(), 1_024_000);
    }

    #[test]
    fn shape_len_rejects_negative_dimensions() {
        assert!(shape_len(&[1, -1, 3]).is_err());
    }
}
