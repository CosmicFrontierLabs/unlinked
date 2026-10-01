//! Only these known wrapper/interface properties may disappear on expansion.
//! Opaque records are refused rather than silently discarded.
pub(super) fn property(key: &str) -> bool {
    matches!(
        key,
        "BlockType"
            | "Name"
            | "SID"
            | "Position"
            | "Ports"
            | "Port"
            | "Orientation"
            | "BlockRotation"
            | "BlockMirror"
            | "ZOrder"
            | "ForegroundColor"
            | "BackgroundColor"
            | "ShowName"
            | "NamePlacement"
            | "DropShadow"
            | "FontSize"
            | "FontName"
            | "FontWeight"
            | "FontAngle"
            | "TreatAsAtomicUnit"
            | "SystemSampleTime"
            | "SFBlockType"
            | "SimViewingDevice"
            | "PermitHierarchicalResolution"
            | "Commented"
            | "SampleTime"
            | "PortDimensions"
            | "OutDataTypeStr"
            | "SignalType"
            | "SamplingMode"
            | "VarSizeSig"
            | "Unit"
            | "OutMin"
            | "OutMax"
            | "BusOutputAsStruct"
            | "OutputFunctionCall"
            | "LatchInputForFeedbackSignals"
            | "LatchByDelayingOutsideSignal"
    )
}

pub(super) fn subset<T: PartialEq>(donor: &[T], base: &[T]) -> bool {
    let mut remaining: Vec<_> = base.iter().collect();
    donor.iter().all(|item| {
        if let Some(index) = remaining.iter().position(|candidate| **candidate == *item) {
            remaining.remove(index);
            true
        } else {
            false
        }
    })
}

pub(super) fn charge(
    budget: &mut usize,
    bytes: usize,
    scans: usize,
) -> Result<(), crate::ImportError> {
    *budget = bytes
        .checked_mul(scans)
        .and_then(|cost| budget.checked_sub(cost))
        .ok_or_else(|| crate::ImportError::Edit("raw expansion work budget exceeded".into()))?;
    Ok(())
}
