use bincode::{Decode, Encode};

pub mod win;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Decode, Encode)]
pub struct Format {
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: SampleFormat,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Decode, Encode)]
pub enum SampleFormat {
    Int8,
    Int16,
    Float32,
}

impl SampleFormat {
    pub fn bits_per_sample(self) -> u16 {
        match self {
            SampleFormat::Int8 => 8,
            SampleFormat::Int16 => 16,
            SampleFormat::Float32 => 32,
        }
    }
}
