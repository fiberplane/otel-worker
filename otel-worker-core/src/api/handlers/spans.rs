use crate::api::errors::{ApiServerError, CommonError};
use crate::api::models::Span;
use crate::data::models::HexEncodedId;
use crate::data::{BoxedStore, DbError};
use axum::extract::{Path, State};
use axum::Json;
use http::StatusCode;
use otel_worker_macros::ApiError;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::error;

#[tracing::instrument(skip_all)]
pub async fn span_get_handler(
    State(store): State<BoxedStore>,
    Path((trace_id, span_id)): Path<(HexEncodedId, HexEncodedId)>,
) -> Result<Json<Span>, ApiServerError<SpanGetError>> {
    let tx = store.start_readonly_transaction().await?;

    let span = store.span_get(&tx, &trace_id, &span_id).await?;

    Ok(Json(span.into()))
}

#[derive(Debug, Serialize, Deserialize, Error, ApiError)]
#[serde(tag = "error", content = "details", rename_all = "camelCase")]
#[non_exhaustive]
pub enum SpanGetError {
    #[api_error(status_code = StatusCode::NOT_FOUND)]
    #[error("Span not found")]
    SpanNotFound,
}

impl From<DbError> for ApiServerError<SpanGetError> {
    fn from(err: DbError) -> Self {
        match err {
            DbError::NotFound => ApiServerError::ServiceError(SpanGetError::SpanNotFound),
            _ => {
                error!(?err, "Failed to get span from database");
                ApiServerError::CommonError(CommonError::InternalServerError)
            }
        }
    }
}

#[tracing::instrument(skip_all)]
pub async fn span_list_handler(
    State(store): State<BoxedStore>,
    Path(trace_id): Path<HexEncodedId>,
) -> Result<Json<Vec<Span>>, ApiServerError<SpanListError>> {
    let tx = store.start_readonly_transaction().await?;

    let spans = store.span_list_by_trace(&tx, &trace_id).await?;
    let spans: Vec<_> = spans.into_iter().map(Into::into).collect();

    Ok(Json(spans))
}

#[derive(Debug, Serialize, Deserialize, Error, ApiError)]
#[serde(tag = "error", content = "details", rename_all = "camelCase")]
#[non_exhaustive]
pub enum SpanListError {}

impl From<DbError> for ApiServerError<SpanListError> {
    fn from(err: DbError) -> Self {
        error!(?err, "Failed to list spans from database");
        ApiServerError::CommonError(CommonError::InternalServerError)
    }
}

#[tracing::instrument(skip_all)]
pub async fn span_delete_handler(
    State(store): State<BoxedStore>,
    Path((trace_id, span_id)): Path<(HexEncodedId, HexEncodedId)>,
) -> Result<StatusCode, ApiServerError<SpanDeleteError>> {
    let tx = store.start_readonly_transaction().await?;

    store.span_delete(&tx, &trace_id, &span_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, Deserialize, Error, ApiError)]
#[serde(tag = "error", content = "details", rename_all = "camelCase")]
#[non_exhaustive]
pub enum SpanDeleteError {}

impl From<DbError> for ApiServerError<SpanDeleteError> {
    fn from(err: DbError) -> Self {
        error!(?err, "Failed to list spans from database");
        ApiServerError::CommonError(CommonError::InternalServerError)
    }
}
