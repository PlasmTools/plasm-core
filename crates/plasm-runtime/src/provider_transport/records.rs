//! Optional durable receipts plus structured telemetry, shared across provider lanes.
use super::{ProviderAttempt, ProviderRecordError};
use std::{io::Write, path::PathBuf};

pub struct ProviderReceiptSink {
    lane: &'static str,
    logical_request_id: String,
    directory: Option<PathBuf>,
}

impl ProviderReceiptSink {
    pub fn new(lane: &'static str, directory: Option<PathBuf>) -> Self {
        Self {
            lane,
            logical_request_id: uuid::Uuid::new_v4().to_string(),
            directory,
        }
    }

    pub fn record(&mut self, attempt: &ProviderAttempt) -> Result<(), ProviderRecordError> {
        let mut record = serde_json::to_value(attempt)?;
        record["lane"] = self.lane.into();
        record["logical_request_id"] = self.logical_request_id.clone().into();
        tracing::debug!(lane = self.lane, receipt = %record, "provider attempt receipt");
        if let Some(directory) = &self.directory {
            std::fs::create_dir_all(directory)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file =
                options.open(directory.join(format!("provider-{}.json", uuid::Uuid::new_v4())))?;
            file.write_all(&serde_json::to_vec(&record)?)?;
            file.sync_all()?;
        }
        Ok(())
    }
}
