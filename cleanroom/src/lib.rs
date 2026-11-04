//! Everything the torch decides, with no hardware in sight. `torch::Torch::tick`
//! is the only entry point the firmware calls, 64 times a second.
#![no_std]
#![forbid(unsafe_code)]

pub mod gesture;
pub mod governor;
pub mod ramp;
pub mod sensors;
pub mod torch;
