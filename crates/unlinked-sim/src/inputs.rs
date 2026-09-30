//! Explicit constant bindings for root Inports. No implicit zero input values.
use super::{block_error, compile, simulate, Error, Graph, Options, Trace};
use std::collections::BTreeMap;
use unlinked_matlab::{
    array_runtime::{Value, ValueKind},
    ArrayBudget,
};
use unlinked_model::{BlockId, Model};

/// Constant external input values, keyed by the original root Inport block ID.
pub type InputValues = BTreeMap<BlockId, Value>;

fn root_input<'a>(model: &'a Model, id: &BlockId) -> Result<&'a unlinked_model::Block, Error> {
    model
        .root
        .blocks
        .iter()
        .find(|b| &b.id == id && b.block_type == "Inport")
        .ok_or_else(|| block_error(&id.0, "input binding must identify a root Inport"))
}
fn validate(id: &BlockId, value: &Value) -> Result<(), Error> {
    value.validate().map_err(|e| block_error(&id.0, e))?;
    if value.kind == ValueKind::Character
        || value.data.is_empty()
        || value.data.len() > 1024
        || value.data.iter().any(|n| !n.is_finite())
    {
        return Err(block_error(
            &id.0,
            "external input must be a finite numeric array with 1 to 1024 elements",
        ));
    }
    Ok(())
}
/// Evaluate pure input expressions in the model workspace, under one shared
/// allocation/operation budget. The expressions cannot execute MATLAB scripts.
pub fn evaluate_inputs(
    model: &Model,
    expressions: &BTreeMap<BlockId, String>,
) -> Result<InputValues, Error> {
    if expressions.len() > 1024 {
        return Err(Error::Options(
            "external input count budget exceeded".into(),
        ));
    }
    let mut budget = ArrayBudget::default();
    let workspace = super::vector::workspace(model, &mut budget)?;
    let mut result = BTreeMap::new();
    let mut elements = 0usize;
    for (id, expression) in expressions {
        root_input(model, id)?;
        let value =
            unlinked_matlab::eval_array_expr_with_budget(expression, &workspace, &mut budget)
                .map_err(|e| block_error(&id.0, e.to_string()))?;
        validate(id, &value)?;
        elements += value.data.len();
        if elements > 100_000 {
            return Err(Error::Options(
                "external input element budget exceeded".into(),
            ));
        }
        result.insert(id.clone(), value);
    }
    Ok(result)
}
/// Compile with explicit constant root inputs. Unbound root inputs still fail.
pub fn compile_with_inputs(
    model: &Model,
    options: &Options,
    inputs: &InputValues,
) -> Result<Graph, Error> {
    if inputs.len() > 1024 || inputs.values().map(|v| v.data.len()).sum::<usize>() > 100_000 {
        return Err(Error::Options("external input budget exceeded".into()));
    }
    for (id, value) in inputs {
        root_input(model, id)?;
        validate(id, value)?;
    }
    let mut budget = ArrayBudget::default();
    let workspace = super::vector::workspace(model, &mut budget)?;
    let mut bound = model.clone();
    for block in &mut bound.root.blocks {
        if let Some(value) = inputs.get(&block.id) {
            for key in ["PortDimensions", "Dimensions"] {
                if let Some(source) = block.param(key) {
                    let dims = unlinked_matlab::eval_array_expr_with_budget(
                        source,
                        &workspace,
                        &mut budget,
                    )
                    .map_err(|e| block_error(&block.id.0, format!("{key}: {e}")))?;
                    let rank = match dims.data.as_slice() {
                        [-1.0] => None,
                        [n] if *n >= 1.0
                            && n.fract() == 0.0
                            && *n == value.data.len() as f64
                            && (value.rows == 1 || value.cols == 1) =>
                        {
                            Some(false)
                        }
                        [rows, cols]
                            if *rows == value.rows as f64 && *cols == value.cols as f64 =>
                        {
                            Some(true)
                        }
                        _ => {
                            return Err(block_error(
                                &block.id.0,
                                "external input shape does not match Inport dimensions",
                            ))
                        }
                    };
                    if let Some(matrix) = rank {
                        block.parameters.insert(
                            "VectorParams1D".into(),
                            if matrix { "off" } else { "on" }.into(),
                        );
                    }
                }
            }
            block.block_type = "Constant".into();
            block
                .parameters
                .insert("Value".into(), super::vector::literal(value));
            // A row/column vector external value follows ordinary Simulink 1-D
            // vector rules unless the original port explicitly preserves rank.
            block.ports.inputs = 0;
        }
    }
    compile(&bound, options)
}
pub fn simulate_model_with_inputs(
    model: &Model,
    options: &Options,
    inputs: &InputValues,
) -> Result<Trace, Error> {
    simulate(&compile_with_inputs(model, options, inputs)?, options)
}
