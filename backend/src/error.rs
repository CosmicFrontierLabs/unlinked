use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use diesel::result::{DatabaseErrorKind, Error as DieselError};
use shared::ErrorResponse;

/// Error type for every `/api` handler; rendered as [`ErrorResponse`] JSON.
///
/// Resources the caller has no role on are reported as [`ApiError::NotFound`]
/// — never `Forbidden` — so their existence is not revealed.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("authentication required")]
    Unauthorized,
    #[error("insufficient role")]
    Forbidden,
    #[error("cross-origin request rejected")]
    CrossOrigin,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden | ApiError::CrossOrigin => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let error = match &self {
            ApiError::Internal(detail) => {
                tracing::error!("{detail}");
                "internal error".to_string()
            }
            other => other.to_string(),
        };
        (status, Json(ErrorResponse { error })).into_response()
    }
}

impl From<DieselError> for ApiError {
    fn from(e: DieselError) -> Self {
        match e {
            DieselError::NotFound => ApiError::NotFound,
            DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => {
                ApiError::Conflict("already exists".to_string())
            }
            other => ApiError::Internal(format!("database error: {other}")),
        }
    }
}

impl From<diesel::r2d2::PoolError> for ApiError {
    fn from(e: diesel::r2d2::PoolError) -> Self {
        ApiError::Internal(format!("database pool error: {e}"))
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
