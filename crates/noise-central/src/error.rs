use axum::{
    Json,
    http::{StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
}

impl ApiError {
    pub const fn bad_request(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
        }
    }

    pub const fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
        }
    }

    pub const fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "authentication_unavailable",
        }
    }

    pub const fn conflict(code: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
        }
    }

    pub const fn not_found(code: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
        }
    }

    pub const fn gone(code: &'static str) -> Self {
        Self {
            status: StatusCode::GONE,
            code,
        }
    }

    pub const fn precondition_required() -> Self {
        Self {
            status: StatusCode::PRECONDITION_REQUIRED,
            code: "if_match_required",
        }
    }

    pub const fn precondition_failed(code: &'static str) -> Self {
        Self {
            status: StatusCode::PRECONDITION_FAILED,
            code,
        }
    }

    pub const fn too_many_requests() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "too_many_challenges",
        }
    }

    pub const fn service_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "service_unavailable",
        }
    }

    pub const fn bad_gateway(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code,
        }
    }

    pub fn database(_error: impl std::fmt::Display) -> Self {
        // Database error details can contain values supplied by a request.
        // Keep the runtime log deliberately generic.
        eprintln!("noise-central database operation failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(CACHE_CONTROL, "no-store")],
            Json(ErrorBody { error: self.code }),
        )
            .into_response()
    }
}
