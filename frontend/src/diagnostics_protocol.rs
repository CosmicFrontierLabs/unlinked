//! One-shot diagnostics worker protocol. JSON strings avoid implicit JS number
//! conversion for u64 generations. Models are never evaluated on the UI thread.
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use unlinked_model::Model;
use unlinked_sim::diagnose::{DiagnosticContext, DiagnosticReport};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub generation: u64,
    pub model: Model,
    pub context: DiagnosticContext,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ready {
        version: u32,
    },
    Report {
        generation: u64,
        report: DiagnosticReport,
    },
    Error {
        /// None only when decoding failed before a trusted generation was available.
        generation: Option<u64>,
        message: String,
    },
}
struct CappedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for CappedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other(format!(
                "diagnostics payload exceeds {} MiB",
                self.limit / (1024 * 1024)
            )));
        }
        let needed = self.bytes.len() + bytes.len();
        if needed > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(needed)
                .min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn encode_request(
    generation: u64,
    model: &Model,
    context: &DiagnosticContext,
) -> Result<String, String> {
    if !unlinked_sim::diagnose::snapshot_shape_bounded(model) {
        return Err("Diagnostics snapshot exceeds shape/depth limits.".into());
    }
    #[derive(Serialize)]
    struct BorrowedRequest<'a> {
        generation: u64,
        model: &'a Model,
        context: &'a DiagnosticContext,
    }
    let mut buffer = CappedBuffer {
        bytes: Vec::new(),
        limit: MAX_REQUEST_BYTES,
    };
    serde_json::to_writer(
        &mut buffer,
        &BorrowedRequest {
            generation,
            model,
            context,
        },
    )
    .map_err(|e| format!("Cannot encode diagnostics request: {e}"))?;
    String::from_utf8(buffer.bytes).map_err(|e| e.to_string())
}
/// Encode with a hard allocation cap. Large reports become a small error carrying
/// the same generation rather than allocating the entire JSON result first.
pub fn encode_response(response: &Response) -> Result<String, String> {
    let mut buffer = CappedBuffer {
        bytes: Vec::new(),
        limit: MAX_RESPONSE_BYTES,
    };
    if serde_json::to_writer(&mut buffer, response).is_err() {
        let generation = match response {
            Response::Report { generation, .. } => Some(*generation),
            Response::Error { generation, .. } => *generation,
            Response::Ready { .. } => None,
        };
        buffer.bytes.clear();
        serde_json::to_writer(
            &mut buffer,
            &Response::Error {
                generation,
                message: "Diagnostics response exceeds the 4 MiB transport limit.".into(),
            },
        )
        .map_err(|e| e.to_string())?;
    }
    String::from_utf8(buffer.bytes).map_err(|e| e.to_string())
}
pub fn decode_request(text: &str) -> Result<Request, String> {
    if text.len() > MAX_REQUEST_BYTES {
        return Err("Diagnostics request exceeds 2 MiB.".into());
    }
    serde_json::from_str(text)
        .map_err(|e| format!("Invalid diagnostics request (JSON nesting limit 128): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> Model {
        Model {
            name: "worker test".into(),
            source: unlinked_model::SourceFormat::Mdl,
            simulink_version: None,
            config: Default::default(),
            root: Default::default(),
            workspace: Default::default(),
            charts: vec![],
            type_defaults: Default::default(),
        }
    }
    #[test]
    fn oversized_response_becomes_a_small_generation_preserving_error() {
        use unlinked_model::validation::{Diagnostic, DiagnosticTarget, Severity};
        let report = DiagnosticReport {
            diagnostics: vec![Diagnostic {
                severity: Severity::Warning,
                code: "large".into(),
                target: DiagnosticTarget::Model,
                message: "x".repeat(MAX_RESPONSE_BYTES),
            }],
            ..Default::default()
        };
        let text = encode_response(&Response::Report {
            generation: u64::MAX,
            report,
        })
        .unwrap();
        assert!(text.len() < 256);
        assert!(
            matches!(serde_json::from_str::<Response>(&text).unwrap(),Response::Error{generation:Some(u64::MAX),message} if message.contains("4 MiB"))
        );
    }
    #[test]
    fn full_width_generation_roundtrips_without_javascript_number_conversion() {
        let text = encode_request(u64::MAX, &model(), &DiagnosticContext::default()).unwrap();
        assert_eq!(decode_request(&text).unwrap().generation, u64::MAX);
    }
    #[test]
    fn oversize_snapshots_and_malformed_requests_fail_explicitly() {
        let mut model = model();
        model
            .workspace
            .insert("oversize".into(), "x".repeat(MAX_REQUEST_BYTES));
        assert!(encode_request(1, &model, &DiagnosticContext::default())
            .unwrap_err()
            .contains("2 MiB"));
        assert!(decode_request("not json")
            .unwrap_err()
            .contains("Invalid diagnostics request"));
        assert!(decode_request(&" ".repeat(MAX_REQUEST_BYTES + 1))
            .unwrap_err()
            .contains("2 MiB"));
    }
    #[test]
    fn callback_text_is_only_serialized_not_executed() {
        let mut model = model();
        model
            .root
            .properties
            .insert("InitFcn".into(), "error('must never execute')".into());
        let text = encode_request(4, &model, &DiagnosticContext::default()).unwrap();
        assert_eq!(
            decode_request(&text).unwrap().model.root.properties["InitFcn"],
            "error('must never execute')"
        );
    }
}
