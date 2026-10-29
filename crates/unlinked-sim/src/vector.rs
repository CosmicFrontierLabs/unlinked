//! Resolve finite signal shapes and lower array-valued models to scalar blocks.
//! Runtime integration remains in the existing scalar engine. Values and matrix
//! coordinates follow MATLAB column-major order; one-dimensional vectors have
//! no row/column orientation. No implicit reshape or matrix-to-vector flattening.
use super::selection::{cell_items, DimensionSelect, MultiPortConfig, SwitchCriteria};
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
/// Array-valued block parameters with their defaults.
type Params = &'static [(&'static str, &'static str)];

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
    /// Zero-input source whose waveform parameters expand elementwise.
    ExpandedSource,
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
    /// Compile-time element selection; one entry per declared dimension.
    Selector {
        dimensions: Vec<DimensionSelect>,
        input_width: Option<usize>,
    },
    /// Runtime data-port selection. With one data port (`index_vector`) the
    /// control indexes the elements of that vector instead.
    MultiPort {
        config: MultiPortConfig,
        index_vector: bool,
    },
    /// Pass input `port` unchanged (Manual Switch, Environment Controller).
    /// `expand` allows scalar expansion against the other inputs.
    Select {
        port: usize,
        expand: bool,
    },
    /// Trigonometric `sincos`: one input, separate sine and cosine outputs.
    SinCos,
    /// Math `transpose`/`hermitian`: a 2-D shape swap, elementwise identity.
    Transpose,
    /// Two vector inputs reduced to one scalar.
    DotProduct,
}
fn output_ports(operation: &Operation) -> usize {
    match operation {
        Operation::Demux { ports, .. } => *ports,
        Operation::SinCos => 2,
        _ => 1,
    }
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
pub(crate) fn supported_native_type(kind: &str) -> bool {
    matches!(
        kind,
        "Constant"
            | "Ground"
            | "Clock"
            | "DigitalClock"
            | "Step"
            | "Sin"
            | "DiscretePulseGenerator"
            | "SignalGenerator"
            | "RandomNumber"
            | "UniformRandomNumber"
            | "Gain"
            | "Bias"
            | "Sum"
            | "Add"
            | "Product"
            | "Integrator"
            | "UnitDelay"
            | "ZeroOrderHold"
            | "Saturate"
            | "Saturation"
            | "DeadZone"
            | "Quantizer"
            | "TransferFcn"
            | "DiscreteTransferFcn"
            | "StateSpace"
            | "Switch"
            | "Logic"
            | "RelationalOperator"
            | "Abs"
            | "Trigonometry"
            | "Math"
            | "Sqrt"
            | "Signum"
            | "Rounding"
            | "UnaryMinus"
            | "Polyval"
            | "DotProduct"
            | "MinMax"
            | "Mux"
            | "Demux"
            | "Scope"
            | "Display"
            | "Inport"
            | "Outport"
            | "Terminator"
            | "ToWorkspace"
            | "Goto"
            | "From"
            | "GotoTagVisibility"
            | "MatlabFunction"
            | "Selector"
            | "MultiPortSwitch"
            | "ManualSwitch"
            // Internal spelling for the library-sourced Environment Controller.
            | "EnvironmentController"
    )
}

/// The internal block type implementing a Simulink library link: the
/// catalog's type key for its `SourceBlock` path (compared with line breaks
/// collapsed, as `catalog::find_library` does). The catalog's library
/// descriptors are exactly the links the simulator lowers, so a palette tile
/// and its simulation support are added together; every other link rejects.
/// These type keys are internal: a file's `BlockType` is never one of them.
pub(crate) fn library_block_type(source: &str) -> Option<&'static str> {
    catalog::find_by_source(source).map(|d| d.type_key)
}
/// Whether a block is a `Reference` to a library block the simulator lowers.
pub(crate) fn supported_library(block: &Block) -> bool {
    block.block_type == "Reference"
        && block
            .library_source
            .as_deref()
            .is_some_and(|s| library_block_type(s).is_some())
}
/// Turn a supported library link into its internal block type, returning
/// whether it was one. Mask parameter values (when the file carries the mask)
/// become ordinary parameters unless the block already stores a value of that
/// name; the link path and mask are then dropped so the block compiles like
/// any other.
fn resolve_library(block: &mut Block) -> bool {
    if !supported_library(block) {
        return false;
    }
    let Some(kind) = block.library_source.as_deref().and_then(library_block_type) else {
        return false;
    };
    if let Some(mask) = block.mask.take() {
        for p in mask.parameters {
            block.parameters.entry(p.name).or_insert(p.value);
        }
    }
    block.block_type = kind.into();
    block.library_source = None;
    true
}

/// Library-linked `Reference` blocks the simulator lowers itself, by their
/// `SourceBlock` path (library names may wrap with newlines in files).
/// Returns the block's library name.
pub(crate) fn supported_library_source(source: &str) -> Option<&'static str> {
    let normalized: String = source.split_whitespace().collect::<Vec<_>>().join(" ");
    match normalized.as_str() {
        "simulink/Signal Routing/Index Vector" => Some("Index Vector"),
        "simulink/Signal Routing/Environment Controller" => Some("Environment Controller"),
        _ => None,
    }
}

/// Rewrite a supported library-linked block into its native equivalent so the
/// ordinary lowering applies. Index Vector is a Multiport Switch with one
/// zero-based data port (<https://www.mathworks.com/help/simulink/slref/indexvector.html>);
/// a linked block stores only the parameters it overrides, so the library's
/// values are filled in when absent. Environment Controller passes its Sim
/// input during simulation (<https://www.mathworks.com/help/simulink/slref/environmentcontroller.html>).
fn lower_library_block(block: &mut Block) {
    if block.block_type != "Reference" || block.mask.is_some() || block.subsystem.is_some() {
        return;
    }
    let Some(kind) = block
        .library_source
        .as_deref()
        .and_then(supported_library_source)
    else {
        return;
    };
    match kind {
        "Index Vector" => {
            block.block_type = "MultiPortSwitch".into();
            block
                .parameters
                .entry("Inputs".into())
                .or_insert_with(|| "1".into());
            block
                .parameters
                .entry("DataPortOrder".into())
                .or_insert_with(|| "Zero-based contiguous".into());
        }
        _ => block.block_type = "EnvironmentController".into(),
    }
    block.library_source = None;
}

/// Zero-based indices from a dialog index expression. `min` is the first
/// valid index in the block's index mode (0 or 1).
fn index_list(
    block: &Block,
    key: &str,
    source: &str,
    min: f64,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<Vec<usize>, Error> {
    let value = eval(block, key, source, workspace, budget)?;
    if value.rows > 1 && value.cols > 1 {
        return Err(block_error(
            &block.id.0,
            format!("{key} must be a scalar or vector of indices"),
        ));
    }
    value
        .data
        .iter()
        .map(|&n| {
            if n.fract() != 0.0 || n < min || n - min >= MAX_ELEMENTS as f64 {
                Err(block_error(
                    &block.id.0,
                    format!(
                        "{key} contains an index {n} outside the {}-based range",
                        min as usize
                    ),
                ))
            } else {
                Ok((n - min) as usize)
            }
        })
        .collect()
}

/// Selector dialog configuration. Index ports and more than two dimensions
/// reject explicitly. <https://www.mathworks.com/help/simulink/slref/selector.html>
fn selector(
    block: &Block,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<Operation, Error> {
    let id = &block.id.0;
    let min = match block
        .param("IndexMode")
        .map(str::trim)
        .unwrap_or("One-based")
    {
        "One-based" => 1.0,
        "Zero-based" => 0.0,
        other => {
            return Err(block_error(
                id,
                format!("unsupported Selector IndexMode={other}"),
            ))
        }
    };
    let input_width = match block.param("InputPortWidth") {
        None => None,
        Some(raw) => {
            let value = eval(block, "InputPortWidth", raw, workspace, budget)?;
            let n = value.data[0];
            if value.data.len() != 1 || n.fract() != 0.0 || (n != -1.0 && n < 1.0) {
                return Err(block_error(
                    id,
                    "Selector InputPortWidth must be -1 or a positive integer",
                ));
            }
            (n != -1.0).then_some(n as usize)
        }
    };
    if block.ports.inputs > 1 {
        return Err(block_error(
            id,
            "Selector index ports are unsupported; indices must come from the dialog",
        ));
    }
    let dimensions = match block.param("NumberOfDimensions").map(str::trim) {
        None | Some("1") => 1,
        Some("2") => 2,
        Some(other) => {
            let message = format!(
                "unsupported Selector NumberOfDimensions={other}; only 1 and 2 are supported"
            );
            return Err(block_error(id, message));
        }
    };
    // Files store the comma-separated `IndexOptions`/`Indices`/`OutputSizes`
    // (every corpus Selector, R2018b-R2020); `IndexOptionArray`/
    // `IndexParamArray`/`OutputSizeArray` are the programmatic names. Both are
    // read. Merged file defaults cannot be told from the instance's own
    // values, so a block carrying both spellings must agree. Returns the
    // items with the spelling they came from, for diagnostics.
    let cells = |file: &'static str,
                 programmatic: &'static str,
                 default: &str|
     -> Result<(Vec<String>, &'static str), Error> {
        let parse = |key: &'static str, raw: &str| {
            cell_items(raw).map_err(|e| block_error(id, format!("{key}: {e}")))
        };
        let stored = |key: &'static str| block.param(key).map(|raw| parse(key, raw)).transpose();
        match (stored(file)?, stored(programmatic)?) {
            (Some(a), Some(b)) if a.iter().map(|s| s.trim()).ne(b.iter().map(|s| s.trim())) => Err(
                block_error(id, format!("Selector {file} and {programmatic} disagree")),
            ),
            (Some(items), _) => Ok((items, file)),
            (None, Some(items)) => Ok((items, programmatic)),
            (None, None) => Ok((parse(file, default)?, file)),
        }
    };
    // Legacy spelling of pre-R2007 files: Elements/InputPortWidth/ElementSrc.
    if block.param("IndexOptionArray").is_none() && block.param("IndexOptions").is_none() {
        if let Some(elements) = block.param("Elements") {
            if block
                .param("ElementSrc")
                .is_some_and(|source| source.trim() != "Internal")
            {
                return Err(block_error(
                    id,
                    "Selector ElementSrc=External (port-driven indices) is unsupported",
                ));
            }
            if dimensions != 1 {
                return Err(block_error(
                    id,
                    "legacy Selector Elements apply to one-dimensional selection only",
                ));
            }
            let indices = index_list(block, "Elements", elements, min, workspace, budget)?;
            return Ok(Operation::Selector {
                dimensions: vec![DimensionSelect::Indices(indices)],
                input_width,
            });
        }
    }
    let (options, options_key) = cells(
        "IndexOptions",
        "IndexOptionArray",
        "{'Index vector (dialog)'}",
    )?;
    let (params, params_key) = cells("Indices", "IndexParamArray", "{'[1 3]'}")?;
    let (sizes, sizes_key) = cells("OutputSizes", "OutputSizeArray", "{'1'}")?;
    if options.len() != dimensions {
        return Err(block_error(
            id,
            format!(
                "Selector {options_key} lists {} options for {dimensions} dimension(s)",
                options.len()
            ),
        ));
    }
    let mut selects = Vec::with_capacity(dimensions);
    for (d, option) in options.iter().enumerate() {
        let param = |list: &[String], key: &str| -> Result<String, Error> {
            list.get(d).cloned().ok_or_else(|| {
                block_error(
                    id,
                    format!("Selector {key} has no entry for dimension {}", d + 1),
                )
            })
        };
        let select = match option.trim() {
            "Select all" => DimensionSelect::All,
            "Index vector (dialog)" => DimensionSelect::Indices(index_list(
                block,
                &format!("{params_key}[{}]", d + 1),
                &param(&params, params_key)?,
                min,
                workspace,
                budget,
            )?),
            "Starting index (dialog)" => {
                let key = format!("{params_key}[{}]", d + 1);
                let start = index_list(
                    block,
                    &key,
                    &param(&params, params_key)?,
                    min,
                    workspace,
                    budget,
                )?;
                let [start] = start[..] else {
                    return Err(block_error(
                        id,
                        format!("{key} must be a single starting index"),
                    ));
                };
                let size_key = format!("{sizes_key}[{}]", d + 1);
                let size = eval(
                    block,
                    &size_key,
                    &param(&sizes, sizes_key)?,
                    workspace,
                    budget,
                )?;
                let n = size.data[0];
                if size.data.len() != 1 || n.fract() != 0.0 || n < 1.0 || n > MAX_ELEMENTS as f64 {
                    return Err(block_error(
                        id,
                        format!("{size_key} must be a positive integer output size"),
                    ));
                }
                let n = n as usize;
                if start + n > MAX_ELEMENTS {
                    return Err(block_error(id, "signal element budget exceeded"));
                }
                DimensionSelect::Indices((start..start + n).collect())
            }
            other @ ("Index vector (port)"
            | "Starting index (port)"
            | "Starting and ending indices (port)") => {
                return Err(block_error(
                    id,
                    format!(
                    "Selector {options_key} entry {other:?} (port-driven indices) is unsupported"
                ),
                ))
            }
            other => {
                return Err(block_error(
                    id,
                    format!("unsupported Selector {options_key} entry {other:?}"),
                ))
            }
        };
        selects.push(select);
    }
    Ok(Operation::Selector {
        dimensions: selects,
        input_width,
    })
}

/// Multiport Switch dialog configuration, including the legacy `zeroidx`
/// spelling of the port order. Absent values use the documented defaults.
/// <https://www.mathworks.com/help/simulink/slref/multiportswitch.html>
fn multiport(
    block: &Block,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<Operation, Error> {
    let id = &block.id.0;
    let n = count(block, "Inputs", "3", workspace, budget)?;
    #[derive(PartialEq)]
    enum Order {
        Zero,
        One,
        Specify,
    }
    let order = match block.param("DataPortOrder").map(str::trim) {
        Some("Zero-based contiguous") => Order::Zero,
        Some("One-based contiguous") => Order::One,
        Some("Specify indices") => Order::Specify,
        Some(other) => {
            return Err(block_error(
                id,
                format!("unsupported MultiPortSwitch DataPortOrder={other}"),
            ))
        }
        None => match block.param("zeroidx").map(str::trim) {
            Some("on") => Order::Zero,
            None | Some("off") => Order::One,
            Some(other) => {
                return Err(block_error(
                    id,
                    format!("unsupported MultiPortSwitch zeroidx={other}"),
                ))
            }
        },
    };
    if block
        .param("AllowDiffInputSizes")
        .is_some_and(|v| v.trim() != "off")
    {
        return Err(block_error(
            id,
            "MultiPortSwitch AllowDiffInputSizes=on (variable-size output) is unsupported",
        ));
    }
    let additional_default_port = match block
        .param("DataPortForDefault")
        .map(str::trim)
        .unwrap_or("Last data port")
    {
        "Last data port" => false,
        "Additional data port" => true,
        other => {
            return Err(block_error(
                id,
                format!("unsupported MultiPortSwitch DataPortForDefault={other}"),
            ))
        }
    };
    let error_on_default = match block
        .param("DiagnosticForDefault")
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
        .unwrap_or("error")
    {
        "error" => true,
        // A warning changes no output value; Unlinked does not surface it.
        "warning" | "none" => false,
        other => {
            return Err(block_error(
                id,
                format!("unsupported MultiPortSwitch DiagnosticForDefault={other}"),
            ))
        }
    };
    let index_vector = n == 1;
    if index_vector {
        if order == Order::Specify {
            return Err(block_error(
                id,
                "MultiPortSwitch with one data port (index vector) requires a contiguous DataPortOrder",
            ));
        }
        if additional_default_port {
            return Err(block_error(
                id,
                "MultiPortSwitch with one data port (index vector) does not support an additional default port",
            ));
        }
    }
    let indices: Vec<Vec<i64>> = match order {
        Order::Zero => (0..n as i64).map(|i| vec![i]).collect(),
        Order::One => (1..=n as i64).map(|i| vec![i]).collect(),
        Order::Specify => {
            let raw = block.param("DataPortIndices").ok_or_else(|| {
                block_error(id, "DataPortOrder=Specify indices requires DataPortIndices")
            })?;
            let items =
                cell_items(raw).map_err(|e| block_error(id, format!("DataPortIndices: {e}")))?;
            if items.len() != n {
                return Err(block_error(
                    id,
                    format!(
                        "DataPortIndices lists {} entries for {n} data ports",
                        items.len()
                    ),
                ));
            }
            let mut seen = BTreeSet::new();
            let mut indices = Vec::with_capacity(n);
            for (k, item) in items.iter().enumerate() {
                let key = format!("DataPortIndices[{}]", k + 1);
                let value = eval(block, &key, item, workspace, budget)?;
                let mut port = Vec::with_capacity(value.data.len());
                for &v in &value.data {
                    if v.fract() != 0.0 || v.abs() > 9_007_199_254_740_992.0 {
                        return Err(block_error(id, format!("{key} must contain integers")));
                    }
                    if !seen.insert(v as i64) {
                        return Err(block_error(
                            id,
                            format!("DataPortIndices assigns control value {v} to more than one data port"),
                        ));
                    }
                    port.push(v as i64);
                }
                indices.push(port);
            }
            indices
        }
    };
    Ok(Operation::MultiPort {
        config: MultiPortConfig {
            indices,
            additional_default_port,
            error_on_default,
        },
        index_vector,
    })
}

/// The lowered scalar Multiport Switch block in the canonical parameter form
/// `import.rs` reads back: explicit indices, default port and diagnostic.
fn canonical_multiport(block: &Block, config: &MultiPortConfig) -> Block {
    let mut scalar = block.clone();
    scalar.block_type = "MultiPortSwitch".into();
    let p = &mut scalar.parameters;
    p.insert("Inputs".into(), config.indices.len().to_string());
    p.insert("DataPortOrder".into(), "Specify indices".into());
    p.insert("DataPortIndices".into(), config.indices_literal());
    p.insert(
        "DataPortForDefault".into(),
        if config.additional_default_port {
            "Additional data port"
        } else {
            "Last data port"
        }
        .into(),
    );
    p.insert(
        "DiagnosticForDefault".into(),
        if config.error_on_default {
            "Error"
        } else {
            "None"
        }
        .into(),
    );
    p.remove("zeroidx");
    scalar
}

fn describe(
    block: &mut Block,
    workspace: &Workspace,
    budget: &mut ArrayBudget,
) -> Result<Spec, Error> {
    // Registered library links become their lowering types (`Ramp`,
    // `ChirpSignal`, `RepeatingSequence`), which are not native block types:
    // Simulink never writes them, so a bare block of such a type is unknown.
    let library = super::library::resolve(block);
    // Index Vector and Environment Controller links rewrite into their
    // native equivalents (`lower_library_block`).
    lower_library_block(block);
    // Catalog library descriptors (the Discontinuities links) become their
    // internal type keys, with the mask's values as parameters.
    let linked = resolve_library(block);
    let id = &block.id.0;
    if block.mask.is_some() || block.library_source.is_some() || block.subsystem.is_some() {
        return Err(block_error(
            id,
            "subsystems, masks and library links require explicit lowering before simulation",
        ));
    }
    if !library && !linked && !supported_native_type(&block.block_type) {
        return Err(block_error(
            id,
            format!("unsupported block type {}", block.block_type),
        ));
    }
    // The Ramp mask's initial output is `X0` in files written by most
    // releases; the current reference documents it as `InitialOutput`.
    if block.block_type == "Ramp" && block.param("X0").is_none() {
        if let Some(initial) = block.param("InitialOutput").map(str::to_owned) {
            block.parameters.insert("X0".into(), initial);
        }
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
    let switch_criteria = if block.block_type == "Switch" {
        Some(SwitchCriteria::parse(block.param("Criteria")).map_err(|e| block_error(id, e))?)
    } else {
        None
    };
    if matches!(
        block.block_type.as_str(),
        "Switch" | "RelationalOperator" | "DeadZone"
    ) && block.param("ZeroCross").is_some_and(|v| v != "off")
    {
        return Err(block_error(
            id,
            "unsupported ZeroCross; event root finding is not implemented",
        ));
    }
    let mut values = BTreeMap::new();
    let (operation, inputs, vector_parameters): (_, _, Params) = match block.block_type.as_str() {
        "Constant" => (Operation::Source, 0, &[("Value", "1")]),
        "RandomNumber"
        | "UniformRandomNumber"
        | "DiscretePulseGenerator"
        | "SignalGenerator"
        | "Ramp"
        | "ChirpSignal" => (Operation::ExpandedSource, 0, &[]),
        "Ground" | "Clock" | "DigitalClock" | "Step" | "Sin" | "RepeatingSequence" => {
            (Operation::Source, 0, &[])
        }
        "DeadZone" => (
            Operation::Elementwise,
            1,
            &[("LowerValue", "-0.5"), ("UpperValue", "0.5")],
        ),
        "Quantizer" => (
            Operation::Elementwise,
            1,
            &[("QuantizationInterval", "0.5")],
        ),
        "CoulombFriction" => (
            Operation::Elementwise,
            1,
            &[("gain", "1"), ("offset", "[1 3 2 0]")],
        ),
        "WrapToZero" => (Operation::Elementwise, 1, &[("Threshold", "255")]),
        "SaturationDynamic" | "DeadZoneDynamic" => (Operation::Elementwise, 3, &[]),
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
            (Operation::Gain { matrix }, 1, &[("Gain", "1")])
        }
        "Integrator" | "UnitDelay" => (Operation::State, 1, &[("InitialCondition", "0")]),
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
                &[],
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
                &[],
            )
        }
        "RelationalOperator" => (Operation::Elementwise, 2, &[]),
        "Switch" => (Operation::Elementwise, 3, &[]),
        "Selector" => (selector(block, workspace, budget)?, 1, &[]),
        "MultiPortSwitch" => {
            let operation = multiport(block, workspace, budget)?;
            let Operation::MultiPort { config, .. } = &operation else {
                unreachable!()
            };
            let inputs = 1 + config.data_ports();
            (operation, inputs, &[])
        }
        "ManualSwitch" => {
            // Position 1 connects the first input, 0 the second (reference
            // page). Files store the position as `CurrentSetting` (the corpus
            // instance and its BlockParameterDefaults use nothing else); `sw`
            // is the programmatic name and wins when both are present. There
            // is no documented default, so an absent value rejects.
            let (key, position) = match (block.param("sw"), block.param("CurrentSetting")) {
                (Some(sw), _) => ("sw", Some(sw)),
                (None, setting) => ("CurrentSetting", setting),
            };
            let port =
                match position.map(str::trim) {
                    Some("1") => 0,
                    Some("0") => 1,
                    Some(other) => {
                        return Err(block_error(
                            id,
                            format!("ManualSwitch {key}={other} must be 0 or 1"),
                        ))
                    }
                    None => return Err(block_error(
                        id,
                        "ManualSwitch requires an explicit position, sw or CurrentSetting (0 or 1)",
                    )),
                };
            if block.param("varsize").is_some_and(|v| v.trim() != "off") {
                return Err(block_error(
                    id,
                    "ManualSwitch varsize=on (inputs differing in size) is unsupported",
                ));
            }
            (Operation::Select { port, expand: true }, 2, &[])
        }
        "EnvironmentController" => (
            Operation::Select {
                port: 0,
                expand: false,
            },
            2,
            &[],
        ),
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
            (Operation::Mux { widths }, n, &[])
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
            (Operation::Demux { widths, ports }, 1, &[])
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
            (Operation::MatlabFunction, block.ports.inputs as usize, &[])
        }
        "TransferFcn" | "DiscreteTransferFcn" | "StateSpace" => (Operation::Transfer, 1, &[]),
        "Trigonometry" => {
            let operator = block.param("Operator").unwrap_or("sin");
            if operator == "sincos" {
                (Operation::SinCos, 1, &[])
            } else {
                let inputs = if super::import::binary_operator("Trigonometry", operator) {
                    2
                } else {
                    1
                };
                (Operation::Elementwise, inputs, &[])
            }
        }
        "Math" => {
            let operator = block.param("Operator").unwrap_or("exp");
            if matches!(operator, "transpose" | "hermitian") {
                (Operation::Transpose, 1, &[])
            } else {
                let inputs = if super::import::binary_operator("Math", operator) {
                    2
                } else {
                    1
                };
                (Operation::Elementwise, inputs, &[])
            }
        }
        "DotProduct" => (Operation::DotProduct, 2, &[]),
        "MinMax" => {
            let n = count(block, "Inputs", "1", workspace, budget)?;
            block.parameters.insert("Inputs".into(), n.to_string());
            (
                if n == 1 {
                    Operation::Reduce
                } else {
                    Operation::Elementwise
                },
                n,
                &[],
            )
        }
        "Bias" | "Saturate" | "Saturation" | "Abs" | "Sqrt" | "Signum" | "Rounding"
        | "UnaryMinus" | "Polyval" | "Scope" | "Display" | "Outport" | "Terminator"
        | "ToWorkspace" | "ZeroOrderHold" => (Operation::Elementwise, 1, &[]),
        other => return Err(block_error(id, format!("unsupported block type {other}"))),
    };
    if output_ports(&operation) == 1 && block.ports.outputs > 1 {
        return Err(block_error(
            id,
            "multiple output ports are supported only for Demux and sincos",
        ));
    }
    for &(key, default) in vector_parameters {
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
    if matches!(operation, Operation::ExpandedSource) {
        let parameters: &[(&str, &str)] = match block.block_type.as_str() {
            "RandomNumber" => &[("Mean", "0"), ("Variance", "1"), ("Seed", "0")],
            "UniformRandomNumber" => &[("Minimum", "-1"), ("Maximum", "1"), ("Seed", "0")],
            "DiscretePulseGenerator" => &[
                ("Amplitude", "1"),
                ("Period", "10"),
                ("PulseWidth", "5"),
                ("PhaseDelay", "0"),
            ],
            "SignalGenerator" => &[("Amplitude", "1"), ("Frequency", "1")],
            "Ramp" => &[("slope", "1"), ("start", "0"), ("X0", "0")],
            "ChirpSignal" => &[("f1", "0.1"), ("T", "100"), ("f2", "1")],
            _ => unreachable!(),
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
        // The threshold is disabled, and not evaluated, for the nonzero criterion.
        "Switch" if switch_criteria.is_some_and(|c| c != SwitchCriteria::Nonzero) => {
            &[("Threshold", "0")]
        }
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
        "RepeatingSequence" => &[("rep_seq_t", "[0 2]"), ("rep_seq_y", "[0 2]")],
        "Polyval" => &[("coefs", unlinked_model::catalog::POLYVAL_DEFAULT_COEFS)],
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
        Operation::ExpandedSource => broadcast(
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
        Operation::Elementwise => {
            // Array parameters expand scalar inputs and must otherwise match
            // the signal, like Simulink's own scalar expansion.
            let parameters = spec
                .values
                .values()
                .map(|value| Some(Shape::from_value(value, true)));
            let output = broadcast(id, inputs.iter().copied().chain(parameters))?;
            // Wrap To Zero documents no expansion of a vector threshold over
            // a scalar input: "the input signal and Threshold parameter must
            // have equal size".
            if block.block_type == "WrapToZero" {
                let threshold = Shape::from_value(&spec.values["Threshold"], true);
                if threshold != Shape::SCALAR && inputs[0].is_some_and(|s| s != threshold) {
                    return Err(block_error(
                        id,
                        "Wrap To Zero requires a scalar Threshold or one matching the input size",
                    ));
                }
            }
            output
        }
        Operation::Selector {
            dimensions,
            input_width,
        } => match inputs[0] {
            None => None,
            Some(input) => Some(selected_shape(id, dimensions, *input_width, input)?),
        },
        Operation::MultiPort { index_vector, .. } => {
            if inputs[0].is_some_and(|s| s != Shape::SCALAR) {
                return Err(block_error(
                    id,
                    "MultiPortSwitch control input must be scalar",
                ));
            }
            if *index_vector {
                match inputs[1] {
                    Some(data) if !data.vector && data.len() > 1 => {
                        return Err(block_error(
                            id,
                            "MultiPortSwitch with one data port indexes a scalar or one-dimensional vector",
                        ))
                    }
                    Some(_) => Some(Shape::SCALAR),
                    None => None,
                }
            } else {
                broadcast(id, inputs[1..].iter().copied())?
            }
        }
        Operation::Select { port, expand } => {
            if *expand {
                broadcast(id, inputs.iter().copied())?
            } else {
                let mut known = inputs.iter().flatten();
                if let Some(first) = known.next() {
                    if known.any(|s| s != first) {
                        return Err(block_error(
                            id,
                            "Environment Controller inputs must have identical dimensions",
                        ));
                    }
                }
                inputs[*port]
            }
        }
        Operation::Transfer | Operation::MatlabFunction => Some(Shape::SCALAR),
        Operation::SinCos => return Ok(vec![inputs[0]; 2]),
        Operation::Transpose => match inputs[0] {
            // A 1-D vector has no orientation to swap, and the Math Function
            // reference does not say whether the result stays 1-D. Require an
            // oriented 1xN/Nx1 matrix rather than guess.
            Some(input) if input.vector => {
                return Err(block_error(
                    id,
                    format!(
                        "Operator={} of an unoriented 1-D vector is ambiguous; use a 1xN or Nx1 matrix signal",
                        block.param("Operator").unwrap_or("transpose")
                    ),
                ))
            }
            Some(input) => Some(Shape {
                rows: input.cols,
                cols: input.rows,
                vector: false,
            }),
            None => None,
        },
        Operation::DotProduct => {
            // https://www.mathworks.com/help/simulink/slref/dotproduct.html:
            // inputs are scalars, vectors or column vectors of equal length;
            // y = sum(u1 .* u2) is always scalar. Expanding a scalar against
            // a vector is an Unlinked extension the reference does not
            // describe.
            let mut length = None;
            for input in inputs.iter().flatten() {
                if !input.vector && input.len() > 1 && input.cols != 1 {
                    return Err(block_error(
                        id,
                        "DotProduct inputs must be scalars, vectors or column vectors",
                    ));
                }
                if input.len() > 1 {
                    if length.is_some_and(|n| n != input.len()) {
                        return Err(block_error(
                            id,
                            "DotProduct inputs must have equal lengths",
                        ));
                    }
                    length = Some(input.len());
                }
            }
            if inputs.iter().any(Option::is_none) {
                None
            } else {
                Some(Shape::SCALAR)
            }
        }
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

/// Resolve a Selector's per-dimension selections against its input shape,
/// returning the zero-based indices along each dimension.
fn selected_indices(
    id: &str,
    dimensions: &[DimensionSelect],
    input_width: Option<usize>,
    input: Shape,
) -> Result<Vec<Vec<usize>>, Error> {
    let extents: Vec<usize> = if dimensions.len() == 1 {
        if !input.vector && input.len() > 1 {
            return Err(block_error(
                id,
                "one-dimensional Selector requires a scalar or vector input; set NumberOfDimensions=2 for matrices",
            ));
        }
        if input_width.is_some_and(|w| w != input.len()) {
            return Err(block_error(
                id,
                format!(
                    "Selector InputPortWidth {} does not match the input width {}",
                    input_width.unwrap(),
                    input.len()
                ),
            ));
        }
        vec![input.len()]
    } else {
        if input.vector {
            return Err(block_error(
                id,
                "two-dimensional Selector requires a matrix input; one-dimensional vectors have no row/column orientation",
            ));
        }
        vec![input.rows, input.cols]
    };
    dimensions
        .iter()
        .zip(&extents)
        .enumerate()
        .map(|(d, (select, &extent))| match select {
            DimensionSelect::All => Ok((0..extent).collect()),
            DimensionSelect::Indices(indices) => {
                if let Some(bad) = indices.iter().find(|&&i| i >= extent) {
                    return Err(block_error(
                        id,
                        format!(
                            "Selector index {} exceeds the input extent {extent} along dimension {}",
                            bad + 1,
                            d + 1
                        ),
                    ));
                }
                Ok(indices.clone())
            }
        })
        .collect()
}
fn selected_shape(
    id: &str,
    dimensions: &[DimensionSelect],
    input_width: Option<usize>,
    input: Shape,
) -> Result<Shape, Error> {
    let indices = selected_indices(id, dimensions, input_width, input)?;
    Ok(if indices.len() == 1 {
        Shape::vector(indices[0].len())
    } else {
        let (rows, cols) = (indices[0].len(), indices[1].len());
        if rows * cols == 1 {
            Shape::SCALAR
        } else {
            Shape {
                rows,
                cols,
                vector: false,
            }
        }
    })
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
    let (mut input_ports, mut output_total, mut parameter_elements) = (0usize, 0usize, 0usize);
    for block in &mut blocks {
        // Files omit parameters at their `BlockParameterDefaults` value, so a
        // Polynomial block without `coefs` takes the file's per-type default
        // before the library default.
        if block.block_type == "Polyval" && block.param("coefs").is_none() {
            if let Some(coefs) = model
                .type_defaults
                .get("Polyval")
                .and_then(|defaults| defaults.get("coefs"))
            {
                block.parameters.insert("coefs".into(), coefs.clone());
            }
        }
        let spec = describe(block, &workspace, &mut budget)?;
        input_ports += spec.inputs;
        output_total += output_ports(&spec.operation);
        parameter_elements += spec.values.values().map(|v| v.data.len()).sum::<usize>();
        if input_ports > MAX_WIRES || output_total > MAX_NODES || parameter_elements > 1_000_000 {
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
        let outputs = output_ports(&specs[source].operation);
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
        .map(|s| vec![None; output_ports(&s.operation)])
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
        if matches!(specs[i].operation, Operation::DotProduct) {
            let length = inputs[i]
                .iter()
                .map(|&(b, p)| shapes[b][p].len())
                .max()
                .unwrap_or(1);
            copies = copies.saturating_mul(length.saturating_add(1));
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
                if let Some(mode) = block
                    .param("CollapseMode")
                    .filter(|v| *v != "All dimensions")
                {
                    return Err(block_error(
                        &block.id.0,
                        format!("unsupported CollapseMode={mode}; only all-dimensions vector reduction is supported"),
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
                } else if matches!(block.block_type.as_str(), "Logic" | "MinMax") {
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
            Operation::Selector {
                dimensions,
                input_width,
            } => {
                let input = input_shapes[0];
                let indices = selected_indices(&block.id.0, dimensions, *input_width, input)?;
                for (j, id) in lowered[i][0].iter().enumerate() {
                    // Column-major element of the input for output element j.
                    let element = if indices.len() == 1 {
                        indices[0][j]
                    } else {
                        let rows = indices[0].len();
                        indices[0][j % rows] + indices[1][j / rows] * input.rows
                    };
                    builder.block(identity(block), id.clone(), name(0, j), 1)?;
                    builder.wire(&input_ids[0][element], id, 0)?;
                }
            }
            Operation::MultiPort {
                config,
                index_vector: true,
            } => {
                // The control selects one element of the single data vector:
                // element k answers to control value base + k, where base is
                // the first contiguous index (0 or 1).
                let elements = input_shapes[1].len();
                let per_element = MultiPortConfig {
                    indices: (0..elements)
                        .map(|k| vec![config.indices[0][0] + k as i64])
                        .collect(),
                    additional_default_port: false,
                    error_on_default: config.error_on_default,
                };
                let id = &lowered[i][0][0];
                builder.block(
                    canonical_multiport(block, &per_element),
                    id.clone(),
                    name(0, 0),
                    1 + elements,
                )?;
                builder.wire(source(0, 0), id, 0)?;
                for (k, input) in input_ids[1].iter().enumerate() {
                    builder.wire(input, id, 1 + k)?;
                }
            }
            Operation::MultiPort {
                config,
                index_vector: false,
            } => {
                for (j, id) in lowered[i][0].iter().enumerate() {
                    builder.block(
                        canonical_multiport(block, config),
                        id.clone(),
                        name(0, j),
                        spec.inputs,
                    )?;
                    builder.wire(source(0, 0), id, 0)?;
                    for port in 1..spec.inputs {
                        builder.wire(source(port, j), id, port)?;
                    }
                }
            }
            Operation::Select { port, .. } => {
                for (j, id) in lowered[i][0].iter().enumerate() {
                    builder.block(identity(block), id.clone(), name(0, j), 1)?;
                    builder.wire(source(*port, j), id, 0)?;
                }
            }
            Operation::SinCos => {
                for (p, operator) in ["sin", "cos"].into_iter().enumerate() {
                    for (j, id) in lowered[i][p].iter().enumerate() {
                        let mut scalar = block.clone();
                        scalar.parameters.insert("Operator".into(), operator.into());
                        builder.block(scalar, id.clone(), name(p, j), 1)?;
                        builder.wire(source(0, j), id, 0)?;
                    }
                }
            }
            Operation::Transpose => {
                let input = input_shapes[0];
                for (j, id) in lowered[i][0].iter().enumerate() {
                    // Output (r, c) is input (c, r); both are column-major.
                    let (row, col) = (j % shape.rows, j / shape.rows);
                    builder.block(block.clone(), id.clone(), name(0, j), 1)?;
                    builder.wire(&input_ids[0][col + row * input.rows], id, 0)?;
                }
            }
            Operation::DotProduct if input_shapes.iter().any(|input| input.len() > 1) => {
                let n = input_shapes.iter().map(|input| input.len()).max().unwrap();
                let id = &lowered[i][0][0];
                let mut sum = block.clone();
                sum.block_type = "Sum".into();
                sum.parameters.insert("Inputs".into(), "+".repeat(n));
                builder.block(sum, id.clone(), name(0, 0), n)?;
                for k in 0..n {
                    let term = builder.id(format!("{}:$dot:{}", id.0, k + 1), false);
                    let mut product = block.clone();
                    product.block_type = "Product".into();
                    product.parameters.insert("Inputs".into(), "**".into());
                    builder.block(
                        product,
                        term.clone(),
                        format!("{}/term{}", name(0, 0), k + 1),
                        2,
                    )?;
                    builder.wire(source(0, k), &term, 0)?;
                    builder.wire(source(1, k), &term, 1)?;
                    builder.wire(&term, id, k)?;
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
