mod win;

use std::time::Duration;

use win::capture::*;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

struct AudioPlayback {
    device: cpal::Device,
    config: cpal::StreamConfig,
}

impl AudioPlayback {
    fn init(channels: u16) -> Result<Self, ()> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(())?;
        let config = device
            .supported_output_configs()
            .map_err(|_| ())?
            .filter(|x| x.channels() == channels)
            .next()
            .ok_or(())?
            // .with_sample_rate(SampleRate(sample_rate))
            .with_max_sample_rate()
            .config();
        Ok(Self { device, config })
    }
}

fn _playback() {
    let channels = 2;
    let playback = AudioPlayback::init(channels).unwrap();

    let sample_rate = playback.config.sample_rate.0;

    let volume = 3000.;
    let mut iter = (0..(4 * sample_rate)).map(move |i| {
        let sample_a: i16 =
            ((i as f32 * 2000. / sample_rate as f32).sin() * volume) as i16;
        let sample_b: i16 =
            ((i as f32 * 4000. / sample_rate as f32).sin() * volume) as i16;
        let sample_c: i16 =
            ((i as f32 * 6000. / sample_rate as f32).sin() * volume) as i16;
        (sample_a + sample_b + sample_c) / 3
    });
    let stream = playback
        .device
        .build_output_stream(
            &playback.config,
            move |data: &mut [i16], _| {
                for d in data.chunks_exact_mut(channels as usize) {
                    let sample = iter.next().unwrap_or_default();
                    for d in d {
                        *d = sample;
                    }
                }
            },
            |err| {
                eprintln!("{:?}", err);
            },
        )
        .unwrap();
    stream.play().unwrap();
    std::thread::sleep(Duration::from_secs(4));
}

fn main() {
    let buffer_duration = Duration::from_millis(100);
    let mut audio_capture = AudioCapture::init(buffer_duration).unwrap();
    let format = audio_capture.format().unwrap();

    println!("channels: {}", format.channels);
    println!("sample rate: {}", format.sample_rate);
    println!("format: {:?}", format.sample_format);

    let actual_duration = Duration::from_secs_f32(
        buffer_duration.as_secs_f32() * audio_capture.buffer_frame_size as f32
            / format.sample_rate as f32
            / 1000.,
    ) / 2;

    if !matches!(format.sample_format, SampleFormat::Float32) {
        todo!("sample formats different than f32");
    }

    let spec = hound::WavSpec {
        channels: format.channels,
        sample_rate: format.sample_rate,
        bits_per_sample: format.sample_format.bits_per_sample(),
        sample_format: format.sample_format.to_hound(),
    };
    let mut writer = hound::WavWriter::create("output.wav", spec).unwrap();

    audio_capture.start().unwrap();
    println!("Started");

    for i in 0..200 {
        print!("{}: data lengths: ", i);
        std::thread::sleep(actual_duration);

        audio_capture
            .read_samples(|data, _| {
                print!("{}, ", data.len() / format.channels as usize);
                for samples in data.chunks_exact(2) {
                    let [left, right]: [f32; 2] = samples.try_into().unwrap();
                    writer.write_sample(left).unwrap();
                    writer.write_sample(right).unwrap();
                }
            })
            .unwrap();

        println!();
    }

    audio_capture.stop().unwrap();
    writer.finalize().unwrap();
    println!("Finalized");
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Format {
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: SampleFormat,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
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

    pub fn to_hound(self) -> hound::SampleFormat {
        match self {
            SampleFormat::Int8 => hound::SampleFormat::Int,
            SampleFormat::Int16 => hound::SampleFormat::Int,
            SampleFormat::Float32 => hound::SampleFormat::Float,
        }
    }
}
