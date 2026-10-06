//! Typed storage faults shared by ports and persistence implementations.
//! This module depends only on concrete external error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TraceSinkStorageError {
    #[error(transparent)]
    Iceberg(#[from] IcebergWriterError),
    #[error(transparent)]
    Projection(#[from] ProjectionError),
}

#[derive(Debug, Error)]
pub enum IcebergWriterError {
    #[error(transparent)]
    Arrow(#[from] datafusion::arrow::error::ArrowError),
    #[error(transparent)]
    DataFusion(#[from] datafusion::error::DataFusionError),
    #[error(transparent)]
    Iceberg(#[from] iceberg_rust::error::Error),
    #[error(transparent)]
    IcebergSpec(#[from] iceberg_rust::spec::error::Error),
    #[error(transparent)]
    IcebergDataFusion(#[from] datafusion_iceberg::error::Error),
    #[error(transparent)]
    SqlCatalog(#[from] iceberg_sql_catalog::error::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    RowDecode(#[from] IcebergRowDecodeError),
    #[error("count query returned an unsupported Arrow column type")]
    UnsupportedCountColumnType,
    #[error("idempotency query result is missing event_id")]
    MissingEventIdColumn,
    #[error("idempotency query event_id column is not UTF-8")]
    EventIdColumnNotUtf8,
    #[error("idempotency query returned a null event_id")]
    NullEventId,
    #[error("idempotency query returned an invalid event_id")]
    InvalidEventId(#[source] uuid::Error),
}

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("trace projection database URL must use postgres:// or postgresql://")]
    UnsupportedCatalogScheme,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("trace detail JSON encoding or decoding failed")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum IcebergRowDecodeError {
    #[error("Iceberg record batch is missing column `{column}`")]
    MissingColumn { column: String },
    #[error("Iceberg column `{column}` is null or missing at row {row}")]
    NullColumn { column: String, row: usize },
    #[error("Iceberg column `{column}` has an unexpected Arrow type; expected {expected}")]
    UnexpectedColumnType {
        column: String,
        expected: &'static str,
    },
    #[error("timestamp column `{column}` is outside the chrono range at row {row}")]
    TimestampOutOfRange { column: String, row: usize },
    #[error("UUID column `{column}` is invalid at row {row}")]
    InvalidUuid {
        column: String,
        row: usize,
        #[source]
        source: uuid::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn row_decode_source_survives_storage_conversion() {
        let source = uuid::Uuid::parse_str("invalid").unwrap_err();
        let error = TraceSinkStorageError::from(IcebergWriterError::from(
            IcebergRowDecodeError::InvalidUuid {
                column: "event_id".into(),
                row: 3,
                source,
            },
        ));
        assert!(error.source().unwrap().is::<uuid::Error>());
        let TraceSinkStorageError::Iceberg(IcebergWriterError::RowDecode(
            IcebergRowDecodeError::InvalidUuid { column, row, .. },
        )) = error
        else {
            panic!("row decode context was lost");
        };
        assert_eq!(column, "event_id");
        assert_eq!(row, 3);
    }

    #[test]
    fn projection_json_source_survives_storage_conversion() {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let error = TraceSinkStorageError::from(ProjectionError::from(source));
        assert!(error.source().unwrap().is::<serde_json::Error>());
        assert!(matches!(
            error,
            TraceSinkStorageError::Projection(ProjectionError::Json(_))
        ));
    }
}
