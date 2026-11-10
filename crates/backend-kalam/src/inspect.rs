//! Count-only, strictly read-only upload-destination inspection. No transfer FD
//! or source/requested-name/size is accepted and no write export is dispatched.
use crate::{Kalam, KalamError, Result, State, input_json, validate_remote_path};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bound directory identity for one count-only native inspection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectUploadDestinationRequest {
    pub storage_id: u32,
    /// Always serialized, including explicit null for root. Unlike download,
    /// None never means "skip parent verification" in this operation.
    pub parent_id: Option<u32>,
    pub destination: String,
}

impl Kalam {
    /// Describes only the loaded ABI. Calls additionally require Ready state.
    pub fn supports_inspect(&self) -> bool {
        self.symbols.safety_version == Some(2) && self.symbols.inspect_upload_destination.is_some()
    }

    /// Invoke only InspectUploadDestination. This does not retry, delete, resume
    /// or dry-run an upload, and cannot recover/clear a prior UnknownOutcome.
    pub fn inspect_upload_destination(
        &mut self,
        request: &InspectUploadDestinationRequest,
    ) -> Result<Value> {
        self.require("InspectUploadDestination", State::Ready)?;
        if !self.supports_inspect() {
            return Err(KalamError::InspectionUnavailable);
        }
        validate_request(request)?;
        let input = input_json(request)?;
        if input.as_bytes().len() > 4096 {
            return Err(KalamError::Input(
                "inspection request exceeds native 4096-byte input limit".into(),
            ));
        }
        let function = self
            .symbols
            .inspect_upload_destination
            .ok_or(KalamError::InspectionUnavailable)?;
        // Use the read-only callback path, never a transfer callback/FD path.
        self.call_with_input(function, &input)
            .and_then(validate_success)
            .inspect_err(|_| self.state = State::Failed)
    }
}

fn validate_request(request: &InspectUploadDestinationRequest) -> Result<()> {
    if request.storage_id == 0 || request.storage_id == u32::MAX {
        return Err(KalamError::Input("reserved inspection storage ID".into()));
    }
    validate_remote_path(&request.destination)?;
    let root = request.parent_id.is_none_or(|id| id == 0 || id == u32::MAX);
    if root != (request.destination == "/") {
        return Err(KalamError::Input(
            "inspection parent ID does not match root/non-root destination".into(),
        ));
    }
    Ok(())
}

fn validate_success(value: Value) -> Result<Value> {
    // This success contract is count-only: do not let unexpected names, paths,
    // object metadata or list/FD fields escape as a successful raw envelope.
    let envelope = value
        .as_object()
        .ok_or_else(|| KalamError::Envelope("inspection envelope must be an object".into()))?;
    if envelope.len() != 3
        || !envelope
            .keys()
            .all(|key| matches!(key.as_str(), "errorType" | "error" | "data"))
    {
        return Err(KalamError::Envelope(
            "inspection envelope has unexpected fields".into(),
        ));
    }
    let data = envelope
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            KalamError::Envelope("inspection data must be a count-only object".into())
        })?;
    if data.len() != 6
        || !data.keys().all(|key| {
            matches!(
                key.as_str(),
                "noWrites"
                    | "complete"
                    | "phase"
                    | "operation"
                    | "destinationChildCount"
                    | "existingHandleCount"
            )
        })
    {
        return Err(KalamError::Envelope(
            "inspection data has unexpected fields".into(),
        ));
    }
    if data.get("noWrites").and_then(Value::as_bool) != Some(true)
        || data.get("complete").and_then(Value::as_bool) != Some(true)
    {
        return Err(KalamError::Envelope(
            "inspection is not a complete no-writes result".into(),
        ));
    }
    // Exact native success labels are finite, bounded UTF-8 categories. Generic
    // malformed-result errors never interpolate untrusted native label strings.
    if data.get("phase").and_then(Value::as_str) != Some("complete")
        || data.get("operation").and_then(Value::as_str) != Some("InspectedUploadDestinationReads")
    {
        return Err(KalamError::Envelope(
            "inspection success labels do not match the read-only contract".into(),
        ));
    }
    if data
        .get("destinationChildCount")
        .and_then(Value::as_u64)
        .is_none()
        || data
            .get("existingHandleCount")
            .and_then(Value::as_u64)
            .is_none()
    {
        return Err(KalamError::Envelope(
            "inspection counters must be precise u64 integers".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn result() -> Value {
        serde_json::json!({"errorType":"", "error":"", "data": {
            "noWrites":true, "complete":true, "phase":"complete",
            "operation":"InspectedUploadDestinationReads",
            "destinationChildCount":0, "existingHandleCount":u64::MAX,
        }})
    }

    #[test]
    fn inspection_request_and_count_contract() {
        let request = InspectUploadDestinationRequest {
            storage_id: 65537,
            parent_id: None,
            destination: "/".into(),
        };
        let json = serde_json::to_value(&request).expect("request JSON");
        assert_eq!(json.as_object().expect("object").len(), 3);
        assert!(json.get("parentId").is_some_and(Value::is_null));
        assert!(validate_request(&request).is_ok());
        let value = validate_success(result()).expect("precise max-u64 count accepted");
        assert_eq!(
            value["data"]["existingHandleCount"].as_u64(),
            Some(u64::MAX)
        );
        for forbidden in ["source", "requestedName", "expectedSize", "fd", "path"] {
            let mut extra = json.clone();
            extra[forbidden] = serde_json::json!("private");
            assert!(serde_json::from_value::<InspectUploadDestinationRequest>(extra).is_err());
        }
        for key in ["noWrites", "complete"] {
            let mut value = result();
            value["data"][key] = serde_json::json!(false);
            assert!(matches!(
                validate_success(value),
                Err(KalamError::Envelope(_))
            ));
        }
        for invalid in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("7"),
            serde_json::json!(null),
        ] {
            let mut value = result();
            value["data"]["existingHandleCount"] = invalid;
            assert!(matches!(
                validate_success(value),
                Err(KalamError::Envelope(_))
            ));
        }
        for key in ["path", "name", "metadata", "files", "fd"] {
            let mut value = result();
            value["data"][key] = serde_json::json!("private/path");
            let error = validate_success(value).expect_err("not count only");
            assert!(!error.to_string().contains("private"));
        }
        let mut value = result();
        value["path"] = serde_json::json!("private/path");
        assert!(validate_success(value).is_err());
    }
}
