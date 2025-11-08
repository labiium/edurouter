use actix_web::{HttpResponse, ResponseError};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("catalog missing model: {0}")]
    UnknownModel(String),
    #[error("alias not registered: {0}")]
    UnknownAlias(String),
    #[error("invalid approval token: {0}")]
    InvalidApproval(String),
    #[error("routing failed: {0}")]
    Planning(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Any(#[from] anyhow::Error),
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: String,
}

impl ResponseError for RouterError {
    fn error_response(&self) -> HttpResponse {
        let code = match self {
            RouterError::UnknownAlias(_) => actix_web::http::StatusCode::BAD_REQUEST,
            RouterError::InvalidApproval(_) => actix_web::http::StatusCode::FORBIDDEN,
            _ => actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = ErrorBody {
            error: self.name(),
            message: self.to_string(),
        };
        HttpResponse::build(code).json(body)
    }
}

impl RouterError {
    fn name(&self) -> &str {
        match self {
            RouterError::UnknownModel(_) => "unknown_model",
            RouterError::UnknownAlias(_) => "unknown_alias",
            RouterError::InvalidApproval(_) => "invalid_approval",
            RouterError::Planning(_) => "planning_error",
            RouterError::Io(_) => "io_error",
            RouterError::Any(_) => "internal_error",
        }
    }
}
