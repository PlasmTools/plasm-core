//! Execute session open, federate, expand, and capability seed application.

mod commit;
mod expand;
mod exposure_replay;
mod federate;
mod open;
mod seeds_apply;

pub use expand::{expand_execute_teaching_session, ExpandTeachingWaveResult};
pub(crate) use exposure_replay::{
    apply_federate_exposure_wave, catalog_waves_from_pairing, replay_teaching_exposure_waves,
    ExposureCatalogWave,
};
pub use federate::federate_execute_session;
pub use open::execute_session_create_response;
pub(crate) use open::execute_session_create_response_inner;
pub use seeds_apply::apply_capability_seeds;
