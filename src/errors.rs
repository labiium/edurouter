use actix_web::{error::JsonPayloadError, http::StatusCode, HttpResponse, ResponseError};
use serde::Serialize;
use std::fmt;
use thiserror::Error;

const ERROR_SCHEMA_VERSION: &str = "1.1";

#[derive(Debug, Clone, Copy)]
pub enum ErrorCode {
    AliasUnknown,
    UnsupportedSchema,
    InvalidApproval,
    InvalidRequest,
    PolicyDeny,
    BudgetExceeded,
    CatalogUnavailable,
    UpstreamUnavailable,
    PlanningFailed,
    InternalError,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::AliasUnknown => "ALIAS_UNKNOWN",
            ErrorCode::UnsupportedSchema => "UNSUPPORTED_SCHEMA",
            ErrorCode::InvalidApproval => "INVALID_APPROVAL",
            ErrorCode::InvalidRequest => "INVALID_REQUEST",
            ErrorCode::PolicyDeny => "POLICY_DENY",
            ErrorCode::BudgetExceeded => "BUDGET_EXCEEDED",
            ErrorCode::CatalogUnavailable => "CATALOG_UNAVAILABLE",
            ErrorCode::UpstreamUnavailable => "UPSTREAM_UNAVAILABLE",
            ErrorCode::PlanningFailed => "PLANNING_FAILED",
            ErrorCode::InternalError => "INTERNAL_ERROR",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ErrorCode::AliasUnknown => StatusCode::NOT_FOUND,
            ErrorCode::UnsupportedSchema => StatusCode::CONFLICT,
            ErrorCode::InvalidApproval => StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
            ErrorCode::PolicyDeny => StatusCode::CONFLICT,
            ErrorCode::BudgetExceeded => StatusCode::PAYMENT_REQUIRED,
            ErrorCode::CatalogUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::UpstreamUnavailable => StatusCode::BAD_GATEWAY,
            ErrorCode::PlanningFailed => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn retry_hint_ms(&self) -> u64 {
        match self {
            ErrorCode::AliasUnknown
            | ErrorCode::UnsupportedSchema
            | ErrorCode::InvalidApproval
            | ErrorCode::InvalidRequest
            | ErrorCode::PolicyDeny => 0,
            ErrorCode::BudgetExceeded => 120_000,
            ErrorCode::CatalogUnavailable
            | ErrorCode::UpstreamUnavailable
            | ErrorCode::PlanningFailed
            | ErrorCode::InternalError => 60_000,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ErrorContext {
    pub request_id: Option<String>,
    pub policy_rev: Option<String>,
}

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("catalog missing model: {0}")]
    UnknownModel(String),
    #[error("alias not registered: {0}")]
    UnknownAlias(String),
    #[error("schema version unsupported: {provided}")]
    UnsupportedSchema {
        provided: String,
        supported: Vec<String>,
    },
    #[error("invalid approval token: {0}")]
    InvalidApproval(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("policy denied: {0}")]
    PolicyDeny(String),
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),
    #[error("catalog unavailable: {0}")]
    CatalogUnavailable(String),
    #[error("upstream unavailable: {0}")]
    UpstreamUnavailable(String),
    #[error("routing failed: {0}")]
    Planning(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Any(#[from] anyhow::Error),
}

impl RouterError {
    pub fn code(&self) -> ErrorCode {
        match self {
            RouterError::UnknownAlias(_) => ErrorCode::AliasUnknown,
            RouterError::UnsupportedSchema { .. } => ErrorCode::UnsupportedSchema,
            RouterError::InvalidApproval(_) => ErrorCode::InvalidApproval,
            RouterError::InvalidRequest(_) => ErrorCode::InvalidRequest,
            RouterError::PolicyDeny(_) => ErrorCode::PolicyDeny,
            RouterError::BudgetExceeded(_) => ErrorCode::BudgetExceeded,
            RouterError::CatalogUnavailable(_) => ErrorCode::CatalogUnavailable,
            RouterError::UpstreamUnavailable(_) => ErrorCode::UpstreamUnavailable,
            RouterError::Planning(_) => ErrorCode::PlanningFailed,
            RouterError::UnknownModel(_) => ErrorCode::InternalError,
            RouterError::Io(_) | RouterError::Any(_) => ErrorCode::InternalError,
        }
    }

    pub fn retry_hint_ms(&self) -> u64 {
        self.code().retry_hint_ms()
    }

    pub fn supported_versions(&self) -> Option<&[String]> {
        match self {
            RouterError::UnsupportedSchema { supported, .. } => Some(supported),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ApiError {
    inner: RouterError,
    context: ErrorContext,
}

impl ApiError {
    pub fn new(inner: RouterError) -> Self {
        Self {
            inner,
            context: ErrorContext::default(),
        }
    }

    pub fn with_context(inner: RouterError, context: ErrorContext) -> Self {
        Self { inner, context }
    }

    pub fn context(mut self, ctx: ErrorContext) -> Self {
        self.context = ctx;
        self
    }
}

impl From<RouterError> for ApiError {
    fn from(value: RouterError) -> Self {
        ApiError::new(value)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        self.inner.code().status()
    }

    fn error_response(&self) -> HttpResponse {
        #[derive(Debug, Serialize)]
        struct ErrorBody {
            schema_version: &'static str,
            code: &'static str,
            message: String,
            request_id: String,
            policy_rev: String,
            retry_hint_ms: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            supported: Option<Vec<String>>,
        }

        let request_id = self
            .context
            .request_id
            .clone()
            .unwrap_or_else(|| "unknown".into());
        let policy_rev = self
            .context
            .policy_rev
            .clone()
            .unwrap_or_else(|| "unknown".into());
        let supported = self.inner.supported_versions().map(|slice| slice.to_vec());
        let body = ErrorBody {
            schema_version: ERROR_SCHEMA_VERSION,
            code: self.inner.code().as_str(),
            message: self.inner.to_string(),
            request_id,
            policy_rev,
            retry_hint_ms: self.inner.retry_hint_ms(),
            supported,
        };
        HttpResponse::build(self.status_code()).json(body)
    }
}

pub fn json_error(err: JsonPayloadError) -> actix_web::Error {
    ApiError::new(RouterError::InvalidRequest(err.to_string())).into()
}

pub fn with_context(
    err: RouterError,
    request_id: Option<String>,
    policy_rev: Option<String>,
) -> ApiError {
    ApiError::with_context(
        err,
        ErrorContext {
            request_id,
            policy_rev,
        },
    )
}
