use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug)]
pub struct OperationResult {
    pub text: String,
    pub source_url: Option<String>,
    pub details: Map<String, Value>,
}

impl OperationResult {
    pub fn new(text: String) -> Self {
        Self {
            text,
            source_url: None,
            details: Map::new(),
        }
    }

    pub fn with_source_url(mut self, source_url: Option<String>) -> Self {
        self.source_url = source_url;
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        if let Value::Object(details) = details {
            self.details = details;
        }
        self
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum BridgeResponse {
    Success {
        ok: bool,
        text: String,
        source_url: Option<String>,
        details: Map<String, Value>,
    },
    Failure {
        ok: bool,
        error: String,
    },
}

impl BridgeResponse {
    pub fn success(result: OperationResult) -> Self {
        let OperationResult {
            text,
            source_url,
            details,
        } = result;
        Self::Success {
            ok: true,
            text,
            source_url,
            details,
        }
    }

    pub fn failure(error: impl ToString) -> Self {
        Self::Failure {
            ok: false,
            error: error.to_string(),
        }
    }
}
