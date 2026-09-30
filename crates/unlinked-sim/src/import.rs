use super::*;
use unlinked_model::{Block, Model, PortKind};

fn parameter(
    block: &Block,
    key: &str,
    default: &str,
    ws: &BTreeMap<String, f64>,
) -> Result<f64, Error> {
    let source = block.param(key).unwrap_or(default);
    let value = unlinked_matlab::eval_expr(source, ws)
        .map_err(|e| block_error(&block.id.0, format!("{key}: {e}")))?;
    if !value.is_finite() {
        return Err(block_error(&block.id.0, format!("{key} must be finite")));
    }
    Ok(value)
}
fn require(block: &Block, key: &str, allowed: &[&str]) -> Result<(), Error> {
    if let Some(value) = block.param(key) {
        if !allowed.contains(&value) {
            return Err(block_error(
                &block.id.0,
                format!("unsupported {key}={value}"),
            ));
        }
    }
    Ok(())
}

fn period_ticks(
    block: &Block,
    options: &Options,
    ws: &BTreeMap<String, f64>,
) -> Result<usize, Error> {
    let value = parameter(
        block,
        "SampleTime",
        if block.block_type == "DigitalClock" {
            "1"
        } else {
            "-1"
        },
        ws,
    )?;
    if value == -1.0 && block.block_type != "DigitalClock" {
        return Ok(1);
    }
    let ticks = value / options.step;
    if !options.step.is_finite()
        || options.step <= 0.0
        || value <= 0.0
        || !(1.0..=1_000_000.0).contains(&ticks.round())
        || !on_grid(ticks)
    {
        return Err(block_error(&block.id.0, "sample time must be a positive integer multiple of simulation step (at most 1,000,000 ticks)"));
    }
    Ok(ticks.round() as usize)
}

/// Compile the root system, lowering ordinary virtual subsystems. Library links,
/// masked/atomic/conditional subsystems and unimplemented block semantics
/// produce diagnostics. Supported bounded vectors/matrices lower to scalar nodes.
/// Workspace expressions are pure numeric MATLAB expressions, never scripts.
pub fn compile(model: &Model, options: &Options) -> Result<Graph, Error> {
    let flattened = super::flatten::flatten(model)?;
    let scalarized = super::vector::scalarize(&flattened)?;
    let model = &scalarized;
    let mut ws = BTreeMap::new();
    for key in model.workspace.keys() {
        if ["pi", "Inf", "NaN", "true", "false"].contains(&key.as_str()) {
            return Err(Error::Parameter(
                key.clone(),
                "overriding built-in constants in model workspace is unsupported".into(),
            ));
        }
    }
    let mut pending = model.workspace.clone();
    while !pending.is_empty() {
        let before = pending.len();
        let mut errors = vec![];
        for (key, source) in pending.clone() {
            match unlinked_matlab::eval_expr(&source, &ws) {
                Ok(value) if value.is_finite() => {
                    ws.insert(key.clone(), value);
                    pending.remove(&key);
                }
                Ok(_) => errors.push((key, "non-finite expression".into())),
                Err(e) => errors.push((key, e.to_string())),
            }
        }
        if before == pending.len() {
            let (key, error) = errors.remove(0);
            return Err(Error::Parameter(key, error));
        }
    }
    // Full Simulink inherited-rate propagation is not implemented. In a
    // multirate model require explicit state-block periods rather than silently
    // assigning the observation rate to an inherited slower signal.
    let mut has_multirate = false;
    let mut inherited_discrete = None;
    for block in &model.root.blocks {
        if matches!(
            block.block_type.as_str(),
            "UnitDelay" | "DiscreteTransferFcn" | "DigitalClock"
        ) {
            has_multirate |= period_ticks(block, options, &ws)? > 1;
            if block.block_type != "DigitalClock"
                && parameter(block, "SampleTime", "-1", &ws)? == -1.0
            {
                inherited_discrete.get_or_insert(block.id.0.clone());
            }
        }
    }
    if has_multirate {
        if let Some(id) = inherited_discrete {
            return Err(block_error(&id, "inherited sample-rate propagation in multirate models is unsupported; specify every discrete state block's SampleTime explicitly"));
        }
    }
    let mut graph = Graph::default();
    let mut reserved = model.root.blocks.iter().map(|b| b.id.0.clone()).collect();
    let mut input_alias = BTreeMap::new();
    let mut direct_discrete = Vec::new();
    for block in &model.root.blocks {
        if graph.nodes.len() >= 100_000 {
            return Err(Error::Options("lowered graph budget exceeded".into()));
        }
        let id = &block.id.0;
        if block.subsystem.is_some() || block.mask.is_some() || block.library_source.is_some() {
            return Err(block_error(
                id,
                "subsystems, masks and library links require explicit lowering before simulation",
            ));
        }
        if block.ports.outputs > 1
            || block.ports.enable
                + block.ports.trigger
                + block.ports.state
                + block.ports.lconn
                + block.ports.rconn
                + block.ports.ifaction
                + block.ports.reset
                > 0
        {
            return Err(block_error(
                id,
                "only one scalar output and ordinary input ports are supported",
            ));
        }
        // Logical outputs are represented by exact scalar 0/1 values.
        // Other blocks retain the existing double-only output restriction.
        let logical_output = matches!(block.block_type.as_str(), "Logic" | "RelationalOperator")
            && block.param("OutDataTypeStr") == Some("boolean");
        if !logical_output {
            require(
                block,
                "OutDataTypeStr",
                &[
                    "Inherit: Inherit via internal rule",
                    "Inherit: Inherit via back propagation",
                    "Inherit: Same as input",
                    "double",
                ],
            )?;
        }
        require(block, "SignalType", &["auto", "real"])?;
        require(block, "SaturateOnIntegerOverflow", &["off"])?;
        let p = |key, default| parameter(block, key, default, &ws);
        let discrete = matches!(
            block.block_type.as_str(),
            "UnitDelay" | "DiscreteTransferFcn" | "DigitalClock"
        );
        let period = if discrete {
            period_ticks(block, options, &ws)?
        } else {
            1
        };
        if let Some(sample) = block.param("SampleTime").filter(|_| !discrete) {
            let value = unlinked_matlab::eval_expr(sample, &ws)
                .map_err(|e| block_error(id, format!("SampleTime: {e}")))?;
            let acceptable = value == -1.0
                || value == 0.0
                || (block.block_type == "Constant" && value == f64::INFINITY);
            if !acceptable {
                return Err(block_error(
                    id,
                    "explicit sample times are supported only on discrete state/source blocks",
                ));
            }
        }
        let kind = match block.block_type.as_str() {
            "TransferFcn" => {
                let input = super::transfer::lower(
                    block,
                    &ws,
                    &mut graph,
                    &mut reserved,
                    &format!("{}/{}", model.name, block.name),
                )?;
                input_alias.insert(id.clone(), input);
                continue;
            }
            "StateSpace" => {
                let input = super::state_space::lower(
                    block,
                    &ws,
                    &mut graph,
                    &mut reserved,
                    &format!("{}/{}", model.name, block.name),
                )?;
                input_alias.insert(id.clone(), input);
                continue;
            }
            "DiscreteTransferFcn" => {
                let first_node = graph.nodes.len();
                let (input, direct) = super::transfer::lower_discrete(
                    block,
                    &ws,
                    &mut graph,
                    &mut reserved,
                    &format!("{}/{}", model.name, block.name),
                )?;
                input_alias.insert(id.clone(), input);
                for node in &mut graph.nodes[first_node..] {
                    if let Kind::UnitDelay { initial } = node.kind {
                        node.kind = Kind::RateDelay {
                            initial,
                            period_ticks: period,
                        };
                    }
                }
                if direct {
                    if graph.nodes.len() >= 100_000 {
                        return Err(Error::Options("lowered graph budget exceeded".into()));
                    }
                    let mut sampled = format!("{id}/sample-input");
                    while !reserved.insert(sampled.clone()) {
                        sampled.push('#');
                    }
                    let output = graph.nodes.iter_mut().find(|n| &n.id == id).unwrap();
                    output.id = sampled.clone();
                    let name = output.name.clone();
                    for wire in &mut graph.wires {
                        if wire.target == *id {
                            wire.target = sampled.clone();
                        }
                        if wire.source == *id {
                            wire.source = sampled.clone();
                        }
                    }
                    graph.nodes.push(Node {
                        id: id.clone(),
                        name,
                        kind: Kind::SampleHold {
                            period_ticks: period,
                        },
                    });
                    graph.wires.push(Wire {
                        source: sampled,
                        target: id.clone(),
                        input: 0,
                    });
                    direct_discrete.push(id.clone());
                }
                continue;
            }
            "Constant" => Kind::Constant {
                value: p("Value", "1")?,
            },
            "Ground" => Kind::Constant { value: 0.0 },
            "Clock" => Kind::Clock,
            "DigitalClock" => Kind::DigitalClock {
                period_ticks: period,
            },
            "Step" => {
                let time = p("Time", "1")?;
                let ticks = (time - options.start) / options.step;
                if time > options.start && time < options.stop && !on_grid(ticks) {
                    return Err(block_error(
                        id,
                        "Step transition must align with fixed-step grid",
                    ));
                }
                Kind::Step {
                    time,
                    before: p("Before", "0")?,
                    after: p("After", "1")?,
                }
            }
            "Sin" => {
                require(block, "SineType", &["Time based"])?;
                require(block, "TimeSource", &["Use simulation time"])?;
                Kind::Sine {
                    amplitude: p("Amplitude", "1")?,
                    frequency: p("Frequency", "1")?,
                    phase: p("Phase", "0")?,
                    bias: p("Bias", "0")?,
                }
            }
            "Gain" => {
                require(block, "Multiplication", &["Element-wise(K.*u)"])?;
                Kind::Gain {
                    gain: p("Gain", "1")?,
                }
            }
            "Bias" => Kind::Bias {
                bias: p("Bias", "0")?,
            },
            "Sum" | "Add" => {
                let raw = block.param("Inputs").unwrap_or("++");
                let signs = if let Ok(count) = raw.parse::<usize>() {
                    if count == 0 || count > 1024 {
                        return Err(block_error(id, "invalid Sum input count"));
                    }
                    vec![1.0; count]
                } else {
                    raw.chars()
                        .filter(|c| !c.is_whitespace() && *c != '|')
                        .map(|c| match c {
                            '+' => Ok(1.0),
                            '-' => Ok(-1.0),
                            _ => Err(block_error(id, "invalid Sum signs")),
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                if signs.is_empty() {
                    return Err(block_error(id, "empty Sum inputs"));
                }
                Kind::Sum { signs }
            }
            "Product" => {
                require(block, "Multiplication", &["Element-wise(.*)"])?;
                let raw = block.param("Inputs").unwrap_or("2");
                let divide = if let Ok(count) = raw.parse::<usize>() {
                    if count == 0 || count > 1024 {
                        return Err(block_error(id, "invalid Product input count"));
                    }
                    vec![false; count]
                } else {
                    raw.chars()
                        .map(|c| match c {
                            '*' => Ok(false),
                            '/' => Ok(true),
                            _ => Err(block_error(id, "invalid Product inputs")),
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                if divide.is_empty() {
                    return Err(block_error(id, "empty Product inputs"));
                }
                Kind::Product { divide }
            }
            "Saturate" | "Saturation" => Kind::Saturation {
                lower: p("LowerLimit", "-0.5")?,
                upper: p("UpperLimit", "0.5")?,
            },
            "Integrator" => {
                require(block, "ExternalReset", &["none"])?;
                require(block, "InitialConditionSource", &["internal"])?;
                require(block, "LimitOutput", &["off"])?;
                require(block, "WrapState", &["off"])?;
                require(block, "ShowStatePort", &["off"])?;
                Kind::Integrator {
                    initial: p("InitialCondition", "0")?,
                }
            }
            "UnitDelay" => Kind::RateDelay {
                initial: p("InitialCondition", "0")?,
                period_ticks: period,
            },
            "Switch" => {
                // Nonzero criteria agrees for both numeric and boolean controls.
                // Threshold criteria need signal datatype propagation: Simulink
                // treats boolean controls specially, so do not guess here.
                if block.param("Criteria") != Some("u2 ~= 0") {
                    return Err(block_error(id, "Switch currently requires Criteria=u2 ~= 0; threshold criteria need datatype propagation"));
                }
                require(block, "ZeroCross", &["off"])?;
                Kind::Switch
            }
            "RelationalOperator" => {
                require(block, "ZeroCross", &["off"])?;
                Kind::Relational {
                    operation: block.param("Operator").unwrap_or(">=").into(),
                }
            }
            "Logic" => {
                let operation = block.param("Operator").unwrap_or("AND").to_string();
                let inputs = if operation == "NOT" {
                    1
                } else {
                    block
                        .param("Inputs")
                        .unwrap_or("2")
                        .parse::<usize>()
                        .map_err(|_| block_error(id, "invalid Logic input count"))?
                };
                Kind::Logic { operation, inputs }
            }
            "Abs" => Kind::Abs,
            "Trigonometry" => Kind::Unary {
                operation: block.param("Operator").unwrap_or("sin").into(),
            },
            "Math" => Kind::Unary {
                operation: block.param("Operator").unwrap_or("exp").into(),
            },
            "Scope" | "Display" | "Outport" | "Terminator" | "ToWorkspace" => {
                if block.ports.inputs > 1 {
                    return Err(block_error(id, "multi-input sinks unsupported"));
                }
                Kind::Sink
            }
            other => return Err(block_error(id, format!("unsupported block type {other}"))),
        };
        graph.nodes.push(Node {
            id: id.clone(),
            name: format!("{}/{}", model.name, block.name),
            kind,
        });
    }
    for connection in model.root.connections() {
        if connection.src.port.kind != PortKind::Out
            || connection.src.port.index != 1
            || connection.dst.port.kind != PortKind::In
            || connection.dst.port.index == 0
        {
            return Err(Error::Connection(
                "only ordinary scalar ports are supported".into(),
            ));
        }
        graph.wires.push(Wire {
            source: connection.src.block.0,
            target: input_alias
                .get(&connection.dst.block.0)
                .cloned()
                .unwrap_or(connection.dst.block.0),
            input: (connection.dst.port.index - 1) as usize,
        });
    }
    super::transfer::validate_discrete_coupling(&graph, &direct_discrete)?;
    Compiled::new(&graph)?;
    Ok(graph)
}

/// Explicit options override imported solver settings. The caller must display
/// the selected solver/step; no claim of matching arbitrary Simulink solvers is made.
pub fn simulate_model(model: &Model, options: &Options) -> Result<Trace, Error> {
    simulate(&compile(model, options)?, options)
}

/// Compile an imported model, then observe samples as they are produced.
pub fn simulate_model_with_observer(
    model: &Model,
    options: &Options,
    observer: impl FnMut(Sample<'_>) -> bool,
) -> Result<Trace, Error> {
    simulate_with_observer(&compile(model, options)?, options, observer)
}
