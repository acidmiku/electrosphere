//! Host adapter connecting briefing requests to recovered integer math.
use crate::{Result, assets::GameData};
use ac3_graphics::scalar::ScalarMath;
use ac3_mission::{BriefingMath, BriefingMathRequest, BriefingScriptError};

pub struct NativeBriefingMath {
    math: ScalarMath,
}

impl NativeBriefingMath {
    pub fn load(data: &GameData) -> Result<Self> {
        Ok(Self {
            math: ScalarMath::from_resident(&data.executable.payload, 0x80010000)?,
        })
    }
}

impl BriefingMath for NativeBriefingMath {
    fn evaluate(
        &self,
        request: BriefingMathRequest,
    ) -> std::result::Result<i32, BriefingScriptError> {
        let result = match request {
            BriefingMathRequest::Atan2 { y, x } => self.math.atan2(y, x),
            BriefingMathRequest::SquareRoot { value } => self.math.square_root(value),
            BriefingMathRequest::Sin { angle } => Ok(self.math.sin(angle)),
            BriefingMathRequest::Cos { angle } => Ok(self.math.cos(angle)),
        };
        result.map_err(|error| BriefingScriptError::InvalidMath {
            request,
            reason: error.to_string(),
        })
    }
}
