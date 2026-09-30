//! Pure scalar MATLAB Function block execution. Programs are parsed once per
//! prepared graph; one shared budget covers every block, sample and solver stage.
use super::{block_error, Error, Graph, Kind};
use std::{cell::RefCell, collections::BTreeMap};
use unlinked_matlab::{
    array_runtime::{Value, ValueKind},
    ArrayBudget, FunctionProgram,
};

pub(super) struct Runtime {
    programs: BTreeMap<usize, FunctionProgram>,
    pub(super) budget: RefCell<ArrayBudget>,
}
impl Runtime {
    pub(super) fn prepare(graph: &Graph) -> Result<Self, Error> {
        let mut programs = BTreeMap::new();
        let mut total_bytes = 0usize;
        for (index, node) in graph.nodes.iter().enumerate() {
            if let Kind::MatlabFunction { script, inputs } = &node.kind {
                total_bytes = total_bytes.saturating_add(script.len());
                if programs.len() >= 1024 || total_bytes > 1_048_576 {
                    return Err(block_error(
                        &node.id,
                        "MATLAB Function graph exceeds 1024 functions or 1 MiB total source",
                    ));
                }
                let program = prepare(&node.id, script, *inputs)?;
                programs.insert(index, program);
            }
        }
        Ok(Self {
            programs,
            budget: RefCell::new(ArrayBudget::default()),
        })
    }
    pub(super) fn evaluate(&self, index: usize, id: &str, args: Vec<f64>) -> Result<f64, Error> {
        let program = self
            .programs
            .get(&index)
            .ok_or_else(|| block_error(id, "missing prepared MATLAB function"))?;
        let output = program
            .evaluate_with_budget(
                args.into_iter().map(Value::scalar).collect(),
                &mut self.budget.borrow_mut(),
            )
            .map_err(|e| block_error(id, format!("MATLAB Function: {e}")))?;
        let value = output
            .first()
            .ok_or_else(|| block_error(id, "MATLAB Function returned no output"))?;
        if output.len() != 1
            || value.kind == ValueKind::Character
            || value.rows != 1
            || value.cols != 1
            || !value.data[0].is_finite()
        {
            return Err(block_error(
                id,
                "MATLAB Function requires one finite real numeric/logical scalar output",
            ));
        }
        Ok(value.data[0])
    }
}
pub(super) fn prepare(id: &str, script: &str, inputs: usize) -> Result<FunctionProgram, Error> {
    if inputs > 256 {
        return Err(block_error(
            id,
            "MATLAB Function supports at most 256 scalar inputs",
        ));
    }
    let program = FunctionProgram::parse(script)
        .map_err(|e| block_error(id, format!("MATLAB Function: {e}")))?;
    let signature = program.signature();
    if signature.inputs.len() != inputs || signature.outputs.len() != 1 {
        return Err(block_error(
            id,
            "MATLAB Function signature must match input ports and declare exactly one output",
        ));
    }
    Ok(program)
}
