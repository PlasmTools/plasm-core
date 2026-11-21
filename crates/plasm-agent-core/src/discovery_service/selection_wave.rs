//! Durable sequencing and usage accounting for one whole-intent selection wave.
use super::decision_checkpoint::DecisionCheckpoint;
use super::{DiscoveryService, DiscoveryStore, Result, matcher, metrics};
use crate::decision_codec::DecisionPacket;

pub(super) struct SelectionWave<'a> {
    service: &'a DiscoveryService,
    checkpoint: &'a DecisionCheckpoint<'a, DiscoveryStore>,
    pub(super) requests: usize,
    pub(super) input_bytes: usize,
    pub(super) tokens: u64,
    pub(super) missing_usage: usize,
}

impl<'a> SelectionWave<'a> {
    pub(super) fn new(
        service: &'a DiscoveryService,
        checkpoint: &'a DecisionCheckpoint<'a, DiscoveryStore>,
    ) -> Self {
        Self {
            service,
            checkpoint,
            requests: 0,
            input_bytes: 0,
            tokens: 0,
            missing_usage: 0,
        }
    }

    pub(super) async fn receive<P: DecisionPacket>(
        &mut self,
        packet: &P,
        body: &str,
        contract: &str,
        usage: impl FnOnce(&P::Response) -> Option<u64>,
    ) -> Result<P::Response> {
        self.checkpoint
            .persist()
            .await
            .map_err(matcher::DiscoveryMatcherError::Checkpoint)?;
        let mut sent = false;
        let response = self
            .checkpoint
            .receive(packet, || async {
                sent = true;
                let started = std::time::Instant::now();
                let result = self.service.contribution_response(body, 1, contract).await;
                metrics::record(metrics::Stage::DecisionProvider, started.elapsed());
                result.map_err(|e| Box::new(e) as matcher::DecisionProviderError)
            })
            .await
            .map_err(matcher::DiscoveryMatcherError::Provider)?;
        if sent {
            let tokens = usage(&response);
            self.requests += 1;
            self.input_bytes += body.len();
            self.tokens += tokens.unwrap_or_default();
            self.missing_usage += usize::from(tokens.is_none());
        }
        Ok(response)
    }
}
