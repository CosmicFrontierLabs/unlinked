//! Resolve finite signal shapes and lower array-valued models to scalar blocks.
//! Runtime integration remains in the existing scalar engine. Values and matrix
//! coordinates follow MATLAB column-major order; one-dimensional vectors have
//! no row/column orientation. No implicit reshape or matrix-to-vector flattening.
use super::{block_error, Error};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use unlinked_matlab::array_runtime::{Value, ValueKind};
use unlinked_matlab::ArrayBudget;
use unlinked_model::*;

const MAX_ELEMENTS: usize = 1024;
const MAX_NODES: usize = 100_000;
const MAX_WIRES: usize = 1_000_000;
const MAX_SHAPE_WORK: usize = 10_000_000;
type Workspace = BTreeMap<String, Value>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    rows: usize,
    cols: usize,
    vector: bool,
}
impl Shape {
    const SCALAR: Self = Self {
        rows: 1,
        cols: 1,
        vector: false,
    };
    fn vector(n: usize) -> Self {
        if n == 1 {
            Self::SCALAR
        } else {
            Self {
                rows: n,
                cols: 1,
                vector: true,
            }
        }
    }
    fn len(self) -> usize {
        self.rows * self.cols
    }
    fn from_value(value: &Value, vector: bool) -> Self {
        if value.data.len() == 1 {
            Self::SCALAR
        } else if vector && (value.rows == 1 || value.cols == 1) {
            Self::vector(value.data.len())
        } else {
            Self {
                rows: value.rows,
                cols: value.cols,
                vector: false,
            }
        }
    }
    fn suffix(self, index: usize) -> String {
        if self.len() == 1 {
            String::new()
        } else if self.vector {
            format!("[{}]", index + 1)
        } else {
            format!("[{},{}]", index % self.rows + 1, index / self.rows + 1)
        }
    }
}
#[derive(Clone)]
enum Operation {
    Source,
    RandomSource,
    Elementwise,
    State,
    Gain {
        matrix: bool,
    },
    Reduce,
    Mux {
        widths: Vec<Option<usize>>,
    },
    Demux {
        widths: Option<Vec<usize>>,
        ports: usize,
    },
    Transfer,
    MatlabFunction,
}
struct Spec {
    operation: Operation,
    inputs: usize,
    values: BTreeMap<String, Value>,
}

fn eval(
    block: &Block,
    key: &str,
    source: &str,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<Value, Error> {
    let value = unlinked_matlab::eval_array_expr_with_budget(source, workspace, budget)
        .map_err(|e| block_error(&block.id.0, format!("{key}: {e}")))?;
    if value.kind == ValueKind::Character
        || value.data.is_empty()
        || value.data.len() > MAX_ELEMENTS
        || value.data.iter().any(|n| !n.is_finite())
    {
        return Err(block_error(
            &block.id.0,
            format!("{key} requires finite numeric data with 1..={MAX_ELEMENTS} elements"),
        ));
    }
    Ok(value)
}
pub(super) fn workspace(model: &Model, budget: &mut ArrayBudget) -> Result<Workspace, Error> {
    let mut result = Workspace::new();
    for key in model.workspace.keys() {
        if ["pi", "Inf", "NaN", "true", "false"].contains(&key.as_str()) {
            return Err(Error::Parameter(
                key.clone(),
                "overriding built-in constants in model workspace is unsupported".into(),
            ));
        }
    }
    let mut pending = model.workspace.clone();
    let mut work = 0;
    let mut elements = 0usize;
    while !pending.is_empty() {
        let before = pending.len();
        let mut first_error = None;
        for (key, source) in pending.clone() {
            work += result.len().max(1);
            if work > 100_000 {
                return Err(Error::Options(
                    "workspace evaluation budget exceeded".into(),
                ));
            }
            match unlinked_matlab::eval_array_expr_with_budget(&source, &result, budget) {
                Ok(value)
                    if value.kind != ValueKind::Character
                        && !value.data.is_empty()
                        && value.data.len() <= MAX_ELEMENTS
                        && value.data.iter().all(|n| n.is_finite()) =>
                {
                    elements += value.data.len();
                    if elements > 100_000 {
                        return Err(Error::Options("workspace element budget exceeded".into()));
                    }
                    result.insert(key.clone(), value);
                    pending.remove(&key);
                }
                Ok(_) => {
                    return Err(Error::Parameter(key, "workspace values must be finite numeric arrays with at most 1024 elements each".into()));
                }
                Err(error) => {
                    first_error.get_or_insert((key, error.to_string()));
                }
            }
        }
        if pending.len() == before {
            let (key, error) = first_error.unwrap();
            return Err(Error::Parameter(key, error));
        }
    }
    Ok(result)
}
fn count(
    block: &Block,
    key: &str,
    default: &str,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<usize, Error> {
    let value = eval(
        block,
        key,
        block.param(key).unwrap_or(default),
        workspace,
        budget,
    )?;
    if value.data.len() != 1
        || value.data[0] < 1.0
        || value.data[0] > MAX_ELEMENTS as f64
        || value.data[0].fract() != 0.0
    {
        return Err(block_error(
            &block.id.0,
            format!("invalid {key} port count"),
        ));
    }
    Ok(value.data[0] as usize)
}
fn signed_count(
    block: &Block,
    product: bool,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<usize, Error> {
    let raw = block
        .param("Inputs")
        .unwrap_or(if product { "2" } else { "++" });
    let signs: Vec<_> = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '|')
        .collect();
    if !signs.is_empty()
        && signs.iter().all(|c| {
            if product {
                ['*', '/'].contains(c)
            } else {
                ['+', '-'].contains(c)
            }
        })
    {
        if signs.len() > MAX_ELEMENTS {
            return Err(block_error(&block.id.0, "input count exceeds limit"));
        }
        Ok(signs.len())
    } else {
        count(block, "Inputs", "2", workspace, budget)
    }
}
fn describe(
    block: &mut Block,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<Spec, Error> {
    let id = &block.id.0;
    if block.mask.is_some() || block.library_source.is_some() || block.subsystem.is_some() {
        return Err(block_error(
            id,
            "subsystems, masks and library links require explicit lowering before simulation",
        ));
    }
    if [
        block.ports.enable,
        block.ports.trigger,
        block.ports.state,
        block.ports.lconn,
        block.ports.rconn,
        block.ports.ifaction,
        block.ports.reset,
    ]
    .iter()
    .any(|&v| v != 0)
    {
        return Err(block_error(
            id,
            "only ordinary input/output ports are supported",
        ));
    }
    if block.block_type == "Switch" && block.param("Criteria") != Some("u2 ~= 0") {
        return Err(block_error(id, "Switch currently requires Criteria=u2 ~= 0; threshold criteria need datatype propagation"));
    }
    if matches!(block.block_type.as_str(), "Switch" | "RelationalOperator")
        && block.param("ZeroCross").is_some_and(|v| v != "off")
    {
        return Err(block_error(
            id,
            "unsupported ZeroCross; event root finding is not implemented",
        ));
    }
    let mut values = BTreeMap::new();
    let (operation, inputs, vector_parameter) = match block.block_type.as_str() {
        "Constant" => (Operation::Source, 0, Some(("Value", "1"))),
        "RandomNumber" | "UniformRandomNumber" => (Operation::RandomSource, 0, None),
        "Ground" | "Clock" | "DigitalClock" | "Step" | "Sin" => (Operation::Source, 0, None),
        "Gain" => {
            let matrix = match block
                .param("Multiplication")
                .unwrap_or("Element-wise(K.*u)")
            {
                "Element-wise(K.*u)" => false,
                "Matrix(K*u)" | "Matrix(K*u) (u vector)" => true,
                other => {
                    return Err(block_error(
                        id,
                        format!("unsupported Multiplication={other}"),
                    ))
                }
            };
            (Operation::Gain { matrix }, 1, Some(("Gain", "1")))
        }
        "Integrator" | "UnitDelay" => (Operation::State, 1, Some(("InitialCondition", "0"))),
        "Sum" | "Add" | "Product" => {
            let product = block.block_type == "Product";
            if product
                && block
                    .param("Multiplication")
                    .is_some_and(|v| v != "Element-wise(.*)")
            {
                return Err(block_error(
                    id,
                    "only elementwise Product multiplication is supported",
                ));
            }
            let n = signed_count(block, product, workspace, budget)?;
            if block.param("Inputs").is_some_and(|v| {
                !v.chars()
                    .filter(|c| !c.is_whitespace() && *c != '|')
                    .all(|c| {
                        if product {
                            c == '*' || c == '/'
                        } else {
                            c == '+' || c == '-'
                        }
                    })
            }) {
                block.parameters.insert("Inputs".into(), n.to_string());
            }
            (
                if n == 1 {
                    Operation::Reduce
                } else {
                    Operation::Elementwise
                },
                n,
                None,
            )
        }
        "Logic" => {
            let not = block.param("Operator") == Some("NOT");
            let n = if not {
                1
            } else {
                count(block, "Inputs", "2", workspace, budget)?
            };
            block.parameters.insert("Inputs".into(), n.to_string());
            (
                if n == 1 && !not {
                    Operation::Reduce
                } else {
                    Operation::Elementwise
                },
                n,
                None,
            )
        }
        "RelationalOperator" => (Operation::Elementwise, 2, None),
        "Switch" => (Operation::Elementwise, 3, None),
        "Mux" => {
            let v = eval(
                block,
                "Inputs",
                block.param("Inputs").unwrap_or("2"),
                workspace,
                budget,
            )?;
            if v.rows > 1 && v.cols > 1 {
                return Err(block_error(id, "Mux input widths must be a vector"));
            }
            let widths = if v.data.len() == 1 {
                vec![None; count(block, "Inputs", "2", workspace, budget)?]
            } else {
                v.data
                    .iter()
                    .map(|&n| {
                        if n == -1.0 {
                            Ok(None)
                        } else if n >= 1.0 && n <= MAX_ELEMENTS as f64 && n.fract() == 0.0 {
                            Ok(Some(n as usize))
                        } else {
                            Err(block_error(id, "invalid Mux input width"))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let n = widths.len();
            (Operation::Mux { widths }, n, None)
        }
        "Demux" => {
            let v = eval(
                block,
                "Outputs",
                block.param("Outputs").unwrap_or("2"),
                workspace,
                budget,
            )?;
            if v.rows > 1 && v.cols > 1 {
                return Err(block_error(id, "Demux output widths must be a vector"));
            }
            let (ports, widths) = if v.data.len() == 1 {
                (count(block, "Outputs", "2", workspace, budget)?, None)
            } else {
                let widths = v
                    .data
                    .iter()
                    .map(|&n| {
                        if n >= 1.0 && n <= MAX_ELEMENTS as f64 && n.fract() == 0.0 {
                            Ok(n as usize)
                        } else {
                            Err(block_error(
                                id,
                                "Demux requires positive explicit output widths",
                            ))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (widths.len(), Some(widths))
            };
            (Operation::Demux { widths, ports }, 1, None)
        }
        "MatlabFunction" => {
            if block.ports.inputs > 256 || block.ports.outputs != 1 {
                return Err(block_error(
                    id,
                    "MATLAB Function requires at most 256 inputs and exactly one output port",
                ));
            }
            if block
                .param("Script")
                .is_none_or(|source| source.len() > 262_144)
            {
                return Err(block_error(
                    id,
                    "missing or oversized MATLAB Function script",
                ));
            }
            (Operation::MatlabFunction, block.ports.inputs as usize, None)
        }
        "TransferFcn" | "DiscreteTransferFcn" | "StateSpace" => (Operation::Transfer, 1, None),
        "Bias" | "Saturate" | "Saturation" | "Abs" | "Trigonometry" | "Math" | "Scope"
        | "Display" | "Outport" | "Terminator" | "ToWorkspace" | "ZeroOrderHold" => {
            (Operation::Elementwise, 1, None)
        }
        other => return Err(block_error(id, format!("unsupported block type {other}"))),
    };
    if !matches!(operation, Operation::Demux { .. }) && block.ports.outputs > 1 {
        return Err(block_error(
            id,
            "multiple output ports are supported only for Demux",
        ));
    }
    if let Some((key, default)) = vector_parameter {
        values.insert(
            key.into(),
            eval(
                block,
                key,
                block.param(key).unwrap_or(default),
                workspace,
                budget,
            )?,
        );
    }
    if matches!(operation, Operation::RandomSource) {
        let parameters: &[(&str, &str)] = if block.block_type == "RandomNumber" {
            &[("Mean", "0"), ("Variance", "1"), ("Seed", "0")]
        } else {
            &[("Minimum", "-1"), ("Maximum", "1"), ("Seed", "0")]
        };
        for &(key, default) in parameters {
            values.insert(
                key.into(),
                eval(
                    block,
                    key,
                    block.param(key).unwrap_or(default),
                    workspace,
                    budget,
                )?,
            );
        }
    }
    let numeric: &[(&str, &str)] = match block.block_type.as_str() {
        "Step" => &[("Time", "1"), ("Before", "0"), ("After", "1")],
        "Sin" => &[
            ("Amplitude", "1"),
            ("Frequency", "1"),
            ("Phase", "0"),
            ("Bias", "0"),
        ],
        "Bias" => &[("Bias", "0")],
        "Saturate" | "Saturation" => &[("LowerLimit", "-0.5"), ("UpperLimit", "0.5")],
        _ => &[],
    };
    for &(key, default) in numeric {
        let value = eval(
            block,
            key,
            block.param(key).unwrap_or(default),
            workspace,
            budget,
        )?;
        if value.data.len() != 1 {
            return Err(block_error(
                id,
                format!("array-valued {key} is unsupported for this block"),
            ));
        }
        block
            .parameters
            .insert(key.into(), value.data[0].to_string());
    }
    let coefficients: &[(&str, &str)] = match block.block_type.as_str() {
        "TransferFcn" => &[("Numerator", "[1]"), ("Denominator", "[1 1]")],
        "DiscreteTransferFcn" => &[
            ("Numerator", "[1]"),
            ("Denominator", "[1 0.5]"),
            ("InitialStates", "0"),
        ],
        "StateSpace" => &[
            ("A", "1"),
            ("B", "1"),
            ("C", "1"),
            ("D", "1"),
            ("InitialCondition", "0"),
        ],
        _ => &[],
    };
    for &(key, default) in coefficients {
        let source = block
            .param(key)
            .or_else(|| {
                if block.block_type == "StateSpace" && key == "InitialCondition" {
                    block.param("X0")
                } else {
                    None
                }
            })
            .unwrap_or(default);
        let value = unlinked_matlab::eval_array_expr_with_budget(source, workspace, budget)
            .map_err(|e| block_error(id, format!("{key}: {e}")))?;
        if value.kind == ValueKind::Character
            || value.data.is_empty()
            || value.data.len() > 4096
            || value.data.iter().any(|v| !v.is_finite())
        {
            return Err(block_error(
                id,
                format!("{key} requires finite numeric data with at most 4096 elements"),
            ));
        }
        block.parameters.insert(key.into(), literal(&value));
    }
    if let Some(sample) = block.param("SampleTime") {
        let value = unlinked_matlab::eval_array_expr_with_budget(sample, workspace, budget)
            .map_err(|e| block_error(id, format!("SampleTime: {e}")))?;
        if value.kind == ValueKind::Character || value.data.len() != 1 || value.data[0].is_nan() {
            return Err(block_error(id, "SampleTime must be a numeric scalar"));
        }
        block.parameters.insert(
            "SampleTime".into(),
            if value.data[0] == f64::INFINITY {
                "Inf".into()
            } else {
                value.data[0].to_string()
            },
        );
    }
    Ok(Spec {
        operation,
        inputs,
        values,
    })
}
pub(super) fn literal(value: &Value) -> String {
    let rows: Vec<_> = (0..value.rows)
        .map(|row| {
            (0..value.cols)
                .map(|col| value.data[col * value.rows + row].to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    format!("[{}]", rows.join(";"))
}
fn broadcast(
    id: &str,
    shapes: impl IntoIterator<Item = Option<Shape>>,
) -> Result<Option<Shape>, Error> {
    let mut result = None;
    for shape in shapes.into_iter().flatten() {
        if let Some(current) = result {
            if current != Shape::SCALAR && shape != Shape::SCALAR && current != shape {
                return Err(block_error(
                    id,
                    format!("incompatible signal shapes {current:?} and {shape:?}"),
                ));
            }
        }
        if result.is_none() || shape != Shape::SCALAR {
            result = Some(shape);
        }
    }
    Ok(result)
}
fn infer(
    block: &Block,
    spec: &Spec,
    inputs: &[Option<Shape>],
) -> Result<Vec<Option<Shape>>, Error> {
    let id = &block.id.0;
    let output = match &spec.operation {
        Operation::Source => Some(
            spec.values
                .get("Value")
                .map(|v| Shape::from_value(v, block.param("VectorParams1D") != Some("off")))
                .unwrap_or(Shape::SCALAR),
        ),
        Operation::RandomSource => broadcast(
            id,
            spec.values.values().map(|value| {
                Some(Shape::from_value(
                    value,
                    block.param("VectorParams1D") != Some("off"),
                ))
            }),
        )?,
        Operation::State => broadcast(
            id,
            [
                inputs[0],
                Some(Shape::from_value(&spec.values["InitialCondition"], true)),
            ],
        )?,
        Operation::Gain { matrix: false } => broadcast(
            id,
            inputs
                .iter()
                .copied()
                .chain([Some(Shape::from_value(&spec.values["Gain"], true))]),
        )?,
        Operation::Gain { matrix: true } => {
            let gain = &spec.values["Gain"];
            if gain.data.len() == 1 {
                inputs[0]
            } else if let Some(input) = inputs[0] {
                let rows = if input.vector {
                    input.len()
                } else {
                    input.rows
                };
                if rows != gain.cols && input != Shape::SCALAR {
                    return Err(block_error(
                        id,
                        "matrix Gain column count does not match input row count",
                    ));
                }
                Some(if input.vector || input == Shape::SCALAR {
                    Shape::vector(gain.rows)
                } else {
                    Shape {
                        rows: gain.rows,
                        cols: input.cols,
                        vector: false,
                    }
                })
            } else {
                None
            }
        }
        Operation::Reduce => inputs[0].map(|_| Shape::SCALAR),
        Operation::Elementwise => broadcast(id, inputs.iter().copied())?,
        Operation::Transfer | Operation::MatlabFunction => Some(Shape::SCALAR),
        Operation::Mux { .. } => {
            if inputs.iter().any(Option::is_none) {
                None
            } else {
                let shapes: Vec<_> = inputs.iter().flatten().copied().collect();
                if shapes.iter().any(|s| !s.vector && s.len() > 1) {
                    return Err(block_error(
                        id,
                        "Mux accepts only scalar or one-dimensional vector inputs",
                    ));
                }
                Some(Shape::vector(shapes.iter().map(|s| s.len()).sum()))
            }
        }
        Operation::Demux { widths, ports } => {
            if let Some(widths) = widths {
                return Ok(widths.iter().map(|&n| Some(Shape::vector(n))).collect());
            }
            let Some(input) = inputs[0] else {
                return Ok(vec![None; *ports]);
            };
            if input.len() % ports != 0 {
                return Err(block_error(id, "equal-width Demux requires input length divisible by output count; specify explicit widths"));
            }
            return Ok(vec![Some(Shape::vector(input.len() / ports)); *ports]);
        }
    };
    if block.block_type == "ZeroOrderHold" && output.is_some_and(|s| s.len() > 1 && !s.vector) {
        return Err(block_error(
            id,
            "ZeroOrderHold supports scalar and one-dimensional vector signals only",
        ));
    }
    if output.is_some_and(|s| s.len() == 0 || s.len() > MAX_ELEMENTS) {
        return Err(block_error(id, "signal element budget exceeded"));
    }
    Ok(vec![output])
}

struct Builder {
    system: System,
    reserved: BTreeSet<String>,
}
impl Builder {
    fn id(&mut self, preferred: String, preserve: bool) -> BlockId {
        if preserve {
            return BlockId(preferred);
        }
        let mut actual = preferred.clone();
        let mut suffix = 0;
        while !self.reserved.insert(actual.clone()) {
            suffix += 1;
            actual = format!("{preferred}#{suffix}");
        }
        BlockId(actual)
    }
    fn block(
        &mut self,
        mut block: Block,
        id: BlockId,
        name: String,
        inputs: usize,
    ) -> Result<(), Error> {
        if self.system.blocks.len() >= MAX_NODES {
            return Err(Error::Options("scalarized node budget exceeded".into()));
        }
        block.id = id;
        block.name = name;
        block.ports = PortCounts::from_slice(&[inputs as u32, 1]);
        self.system.blocks.push(block);
        Ok(())
    }
    fn wire(&mut self, source: &BlockId, target: &BlockId, input: usize) -> Result<(), Error> {
        if self.system.lines.len() >= MAX_WIRES {
            return Err(Error::Options("scalarized wire budget exceeded".into()));
        }
        self.system.lines.push(Line {
            src: Some(Endpoint {
                block: source.clone(),
                port: PortRef {
                    kind: PortKind::Out,
                    index: 1,
                },
            }),
            dst: Some(Endpoint {
                block: target.clone(),
                port: PortRef {
                    kind: PortKind::In,
                    index: input as u32 + 1,
                },
            }),
            ..Line::default()
        });
        Ok(())
    }
}
fn identity(block: &Block) -> Block {
    let mut result = block.clone();
    result.block_type = "Gain".into();
    result.parameters.insert("Gain".into(), "1".into());
    result
        .parameters
        .insert("Multiplication".into(), "Element-wise(K.*u)".into());
    result
}

/// Scalarize a flat imported model. Unsupported block and parameter semantics
/// remain subject to the scalar compiler after dimensional lowering.
pub(super) fn scalarize(model: &Model) -> Result<Model, Error> {
    if model.root.blocks.len() > MAX_NODES {
        return Err(Error::Options("model node budget exceeded".into()));
    }
    let function_blocks: Vec<_> = model
        .root
        .blocks
        .iter()
        .filter(|b| b.block_type == "MatlabFunction")
        .collect();
    if function_blocks.len() > 1024
        || function_blocks.iter().fold(0usize, |total, b| {
            total.saturating_add(b.param("Script").map_or(0, str::len))
        }) > 1_048_576
    {
        return Err(Error::Options(
            "MATLAB Function graph exceeds 1024 functions or 1 MiB total source".into(),
        ));
    }
    let mut budget = ArrayBudget::default();
    let workspace = workspace(model, &mut budget)?;
    let mut blocks = model.root.blocks.clone();
    let mut ids = BTreeMap::new();
    for (i, block) in blocks.iter().enumerate() {
        if ids.insert(block.id.clone(), i).is_some() {
            return Err(block_error(&block.id.0, "duplicate block ID"));
        }
    }
    if workspace.len().saturating_mul(blocks.len()) > MAX_SHAPE_WORK {
        return Err(Error::Options(
            "parameter environment evaluation budget exceeded".into(),
        ));
    }
    let mut specs = Vec::new();
    let (mut input_ports, mut output_ports, mut parameter_elements) = (0usize, 0usize, 0usize);
    for block in &mut blocks {
        let spec = describe(block, &workspace, &mut budget)?;
        input_ports += spec.inputs;
        output_ports += match spec.operation {
            Operation::Demux { ports, .. } => ports,
            _ => 1,
        };
        parameter_elements += spec.values.values().map(|v| v.data.len()).sum::<usize>();
        if input_ports > MAX_WIRES || output_ports > MAX_NODES || parameter_elements > 1_000_000 {
            return Err(Error::Options(
                "scalarization port/parameter budget exceeded".into(),
            ));
        }
        specs.push(spec);
    }
    // One source block/output port for every destination input port.
    let mut inputs: Vec<Vec<Option<(usize, usize)>>> =
        specs.iter().map(|s| vec![None; s.inputs]).collect();
    let connections = model.root.connections();
    if connections.len() > MAX_WIRES {
        return Err(Error::Options("model wire budget exceeded".into()));
    }
    for connection in connections {
        if connection.src.port.kind != PortKind::Out
            || connection.dst.port.kind != PortKind::In
            || connection.src.port.index == 0
            || connection.dst.port.index == 0
        {
            return Err(Error::Connection(
                "only ordinary 1-based input/output ports are supported".into(),
            ));
        }
        let source = *ids
            .get(&connection.src.block)
            .ok_or_else(|| Error::Connection("unknown source block".into()))?;
        let target = *ids
            .get(&connection.dst.block)
            .ok_or_else(|| Error::Connection("unknown target block".into()))?;
        let outputs = match specs[source].operation {
            Operation::Demux { ports, .. } => ports,
            _ => 1,
        };
        let port = connection.src.port.index as usize - 1;
        if port >= outputs {
            return Err(Error::Connection(
                "source output port is out of range".into(),
            ));
        }
        let target_port = inputs[target]
            .get_mut(connection.dst.port.index as usize - 1)
            .ok_or_else(|| Error::Connection("target input port is out of range".into()))?;
        if target_port.replace((source, port)).is_some() {
            return Err(Error::Connection("multiple drivers of input port".into()));
        }
    }
    let inputs: Vec<Vec<(usize, usize)>> = inputs
        .into_iter()
        .enumerate()
        .map(|(i, ports)| {
            ports
                .into_iter()
                .enumerate()
                .map(|(p, v)| {
                    v.ok_or_else(|| {
                        block_error(&blocks[i].id.0, format!("unconnected input {}", p + 1))
                    })
                })
                .collect()
        })
        .collect::<Result<_, _>>()?;
    let mut shapes: Vec<Vec<Option<Shape>>> = specs
        .iter()
        .map(|s| {
            vec![
                None;
                match s.operation {
                    Operation::Demux { ports, .. } => ports,
                    _ => 1,
                }
            ]
        })
        .collect();
    // Revisit consumers only when their input dimensions change. Delay errors
    // from provisional dimensions until all feedback constraints have settled.
    let mut consumers = vec![Vec::new(); blocks.len()];
    for (target, ports) in inputs.iter().enumerate() {
        for &(source, _) in ports {
            consumers[source].push(target);
        }
    }
    let mut queue: VecDeque<_> = (0..blocks.len()).collect();
    let mut queued = vec![true; blocks.len()];
    let mut work = 0usize;
    while let Some(i) = queue.pop_front() {
        queued[i] = false;
        work += inputs[i].len().max(1) + shapes[i].len();
        if work > MAX_SHAPE_WORK {
            return Err(Error::Options(
                "signal shape propagation budget exceeded or cyclic shape constraints".into(),
            ));
        }
        let input_shapes: Vec<_> = inputs[i].iter().map(|&(b, p)| shapes[b][p]).collect();
        if let Ok(next) = infer(&blocks[i], &specs[i], &input_shapes) {
            if next != shapes[i] {
                shapes[i] = next;
                for &target in &consumers[i] {
                    if !queued[target] {
                        queue.push_back(target);
                        queued[target] = true;
                    }
                }
            }
        }
    }
    for (i, block) in blocks.iter().enumerate() {
        let input_shapes: Vec<_> = inputs[i].iter().map(|&(b, p)| shapes[b][p]).collect();
        infer(block, &specs[i], &input_shapes)?;
    }
    let shapes: Vec<Vec<Shape>> = shapes.into_iter().enumerate().map(|(i,ports)| ports.into_iter().map(|v| v.ok_or_else(||block_error(&blocks[i].id.0,"unresolved signal shape; provide a source or initial condition with known dimensions"))).collect()).collect::<Result<_,_>>()?;
    let output_elements: usize = shapes.iter().flatten().map(|s| s.len()).sum();
    if output_elements > MAX_NODES {
        return Err(Error::Options(
            "scalarized output element budget exceeded".into(),
        ));
    }
    let mut expanded_text = 0usize;
    for (i, block) in blocks.iter().enumerate() {
        let bytes = block
            .parameters
            .iter()
            .map(|(k, v)| k.len().saturating_add(v.len()))
            .sum::<usize>()
            .saturating_add(block.name.len())
            .saturating_add(block.id.0.len());
        let mut copies = shapes[i].iter().map(|s| s.len()).sum::<usize>();
        if matches!(specs[i].operation, Operation::Gain { matrix: true }) {
            copies = copies.saturating_mul(specs[i].values["Gain"].cols.saturating_add(1));
        }
        expanded_text = expanded_text.saturating_add(bytes.saturating_mul(copies));
        if expanded_text > 64 * 1024 * 1024 {
            return Err(Error::Options(
                "scalarized parameter text budget exceeded".into(),
            ));
        }
    }
    let mut builder = Builder {
        system: System::default(),
        reserved: blocks.iter().map(|b| b.id.0.clone()).collect(),
    };
    let mut lowered = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        let mut ports = Vec::new();
        for (p, &shape) in shapes[i].iter().enumerate() {
            if shape.len() == 0 || shape.len() > MAX_ELEMENTS {
                return Err(block_error(&block.id.0, "signal element budget exceeded"));
            }
            let prefix = if shapes[i].len() == 1 {
                block.id.0.clone()
            } else {
                format!("{}:out:{}", block.id.0, p + 1)
            };
            ports.push(
                (0..shape.len())
                    .map(|j| {
                        builder.id(
                            format!("{prefix}{}", shape.suffix(j)),
                            shapes[i].len() == 1 && shape.len() == 1,
                        )
                    })
                    .collect::<Vec<_>>(),
            );
        }
        lowered.push(ports);
    }
    for (i, block) in blocks.iter().enumerate() {
        let spec = &specs[i];
        let input_shapes: Vec<_> = inputs[i].iter().map(|&(b, p)| shapes[b][p]).collect();
        let input_ids: Vec<_> = inputs[i].iter().map(|&(b, p)| &lowered[b][p]).collect();
        let source = |port: usize, element: usize| -> &BlockId {
            &input_ids[port][if input_shapes[port].len() == 1 {
                0
            } else {
                element
            }]
        };
        let shape = shapes[i][0];
        let name = |port: usize, element: usize| {
            let prefix = if shapes[i].len() == 1 {
                block.name.clone()
            } else {
                format!("{}/out{}", block.name, port + 1)
            };
            format!("{prefix}{}", shapes[i][port].suffix(element))
        };
        match &spec.operation {
            Operation::Mux { widths } => {
                for (p, width) in widths.iter().enumerate() {
                    if width.is_some_and(|n| n != input_shapes[p].len()) {
                        return Err(block_error(
                            &block.id.0,
                            "Mux input width does not match specified width",
                        ));
                    }
                }
                let mut offset = 0;
                for (port, input_shape) in input_shapes.iter().enumerate() {
                    for element in 0..input_shape.len() {
                        let id = &lowered[i][0][offset];
                        builder.block(identity(block), id.clone(), name(0, offset), 1)?;
                        builder.wire(source(port, element), id, 0)?;
                        offset += 1;
                    }
                }
            }
            Operation::Demux { .. } => {
                if (!input_shapes[0].vector && input_shapes[0].len() > 1)
                    || shapes[i].iter().map(|s| s.len()).sum::<usize>() != input_shapes[0].len()
                {
                    return Err(block_error(
                        &block.id.0,
                        "Demux requires a vector matching total output widths",
                    ));
                }
                let mut offset = 0;
                for (p, port) in lowered[i].iter().enumerate() {
                    for (j, id) in port.iter().enumerate() {
                        builder.block(identity(block), id.clone(), name(p, j), 1)?;
                        builder.wire(&input_ids[0][offset], id, 0)?;
                        offset += 1;
                    }
                }
            }
            Operation::Gain { matrix: true } if spec.values["Gain"].data.len() > 1 => {
                let gain = &spec.values["Gain"];
                let input = input_shapes[0];
                if block.param("Multiplication") == Some("Matrix(K*u) (u vector)")
                    && !input.vector
                    && input.len() > 1
                {
                    return Err(block_error(
                        &block.id.0,
                        "matrix Gain u-vector mode requires a one-dimensional input",
                    ));
                }
                if (if input.vector {
                    input.len()
                } else {
                    input.rows
                }) != gain.cols
                {
                    return Err(block_error(
                        &block.id.0,
                        "matrix Gain dimensions are incompatible",
                    ));
                }
                for (j, id) in lowered[i][0].iter().enumerate() {
                    let row = j % gain.rows;
                    let col = j / gain.rows;
                    let mut sum = block.clone();
                    sum.block_type = "Sum".into();
                    sum.parameters
                        .insert("Inputs".into(), gain.cols.to_string());
                    builder.block(sum, id.clone(), name(0, j), gain.cols)?;
                    for k in 0..gain.cols {
                        let term = builder.id(format!("{}:$gain:{}", id.0, k + 1), false);
                        let mut weighted = identity(block);
                        weighted
                            .parameters
                            .insert("Gain".into(), gain.data[row + k * gain.rows].to_string());
                        builder.block(
                            weighted,
                            term.clone(),
                            format!("{}/term{}", name(0, j), k + 1),
                            1,
                        )?;
                        builder.wire(&input_ids[0][k + col * gain.cols], &term, 0)?;
                        builder.wire(&term, id, k)?;
                    }
                }
            }
            Operation::Reduce if input_shapes[0].len() > 1 => {
                if !input_shapes[0].vector {
                    return Err(block_error(
                        &block.id.0,
                        "single-input reduction currently requires a one-dimensional vector",
                    ));
                }
                if block
                    .param("CollapseMode")
                    .is_some_and(|v| v != "All dimensions")
                {
                    return Err(block_error(
                        &block.id.0,
                        "only all-dimensions vector reduction is supported",
                    ));
                }
                let n = input_shapes[0].len();
                let id = &lowered[i][0][0];
                let mut reduced = block.clone();
                let operator: String = block
                    .param("Inputs")
                    .unwrap_or("1")
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != '|')
                    .collect();
                let spec = if block.block_type == "Product" {
                    if operator == "/" {
                        "/".repeat(n)
                    } else {
                        "*".repeat(n)
                    }
                } else if block.block_type == "Logic" {
                    n.to_string()
                } else if operator == "-" {
                    "-".repeat(n)
                } else {
                    "+".repeat(n)
                };
                reduced.parameters.insert("Inputs".into(), spec);
                builder.block(reduced, id.clone(), name(0, 0), n)?;
                for (k, input) in input_ids[0].iter().enumerate() {
                    builder.wire(input, id, k)?;
                }
            }
            _ => {
                if matches!(spec.operation, Operation::MatlabFunction)
                    && input_shapes.iter().any(|shape| *shape != Shape::SCALAR)
                {
                    return Err(block_error(
                        &block.id.0,
                        "MATLAB Function inputs must be scalar",
                    ));
                }
                if matches!(spec.operation, Operation::Transfer) && input_shapes[0] != Shape::SCALAR
                {
                    return Err(block_error(
                        &block.id.0,
                        "TransferFcn remains scalar SISO only",
                    ));
                }
                if matches!(spec.operation, Operation::State) {
                    let initial = Shape::from_value(&spec.values["InitialCondition"], true);
                    if initial != Shape::SCALAR && initial != shape {
                        return Err(block_error(
                            &block.id.0,
                            "initial-condition shape does not match state input",
                        ));
                    }
                }
                for (j, id) in lowered[i][0].iter().enumerate() {
                    let mut scalar = block.clone();
                    for (key, value) in &spec.values {
                        let index = if value.data.len() == 1 { 0 } else { j };
                        scalar
                            .parameters
                            .insert(key.clone(), value.data[index].to_string());
                    }
                    if matches!(spec.operation, Operation::Gain { .. }) {
                        scalar
                            .parameters
                            .insert("Multiplication".into(), "Element-wise(K.*u)".into());
                    }
                    builder.block(scalar, id.clone(), name(0, j), spec.inputs)?;
                    for port in 0..spec.inputs {
                        builder.wire(source(port, j), id, port)?;
                    }
                }
            }
        }
    }
    let mut result = model.clone();
    result.root = builder.system;
    result.workspace = workspace
        .into_iter()
        .filter_map(|(key, value)| {
            (value.data.len() == 1).then(|| (key, value.data[0].to_string()))
        })
        .collect();
    Ok(result)
}
