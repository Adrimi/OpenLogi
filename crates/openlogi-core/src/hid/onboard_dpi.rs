//! DPI stages stored on the mouse itself — pure data, no I/O.
//!
//! Some mice (the Compx family) keep a short list of DPI stages in their own
//! flash. The DPI button steps through them and the LED shows the active
//! stage's colour, with no software running. The driver that reads and writes
//! them lives in `openlogi_device::compx`.

use serde::{Deserialize, Serialize};

use super::Dpi;
use crate::color::Rgb;

/// The `feature_hex` of the [`WriteError::FeatureUnsupported`] a route without
/// onboard DPI stages answers with. These stages are no HID++ feature, so no
/// real feature ID fits; zero names none.
///
/// [`WriteError::FeatureUnsupported`]: super::WriteError::FeatureUnsupported
pub const NO_ONBOARD_STAGES_FEATURE: u16 = 0x0000;

/// One onboard DPI stage: the resolution on both axes and the LED colour that
/// marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardDpiStage {
    /// Resolution, the same on both axes.
    pub dpi: Dpi,
    /// LED colour shown while the stage is active.
    pub color: Rgb,
}

/// The stages a mouse stores, as it reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardDpiStages {
    /// The configured stages in button order; never empty.
    pub stages: Vec<OnboardDpiStage>,
    /// Zero-based index of the active stage in [`Self::stages`].
    pub active: u8,
    /// The most stages the mouse stores.
    pub max_stages: u8,
}

impl OnboardDpiStages {
    /// The active stage, or `None` when [`Self::active`] lies outside the
    /// list.
    #[must_use]
    pub fn active_stage(&self) -> Option<&OnboardDpiStage> {
        self.stages.get(usize::from(self.active))
    }
}

/// A replacement for the stored stage list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardDpiStagesChange {
    /// The stages to store, in button order; one to the mouse's maximum.
    pub stages: Vec<OnboardDpiStage>,
    /// Zero-based index of the stage to activate.
    pub active: u8,
}

#[cfg(test)]
mod tests {
    use super::{OnboardDpiStage, OnboardDpiStages};
    use crate::color::Rgb;
    use crate::hid::Dpi;

    #[test]
    fn the_active_stage_is_looked_up_by_index() {
        let stage = |dpi| OnboardDpiStage {
            dpi: Dpi::new(dpi),
            color: Rgb::WHITE,
        };
        let mut stages = OnboardDpiStages {
            stages: vec![stage(800), stage(1600)],
            active: 1,
            max_stages: 8,
        };
        assert_eq!(stages.active_stage(), Some(&stage(1600)));
        stages.active = 2;
        assert_eq!(stages.active_stage(), None);
    }
}
