use std::{
    collections::VecDeque,
    fmt,
    net::{IpAddr, Ipv4Addr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

use bincode::{config::Configuration, error::EncodeError, Decode, Encode};
use earplugs::{win::capture::*, Format, SampleFormat};
use structopt::StructOpt;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

struct AudioPlayback {
    device: cpal::Device,
    config: cpal::StreamConfig,
}

#[derive(Debug)]
struct AudioPlaybackInitError;

impl fmt::Display for AudioPlaybackInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AudioPlaybackInitError")
    }
}

impl std::error::Error for AudioPlaybackInitError {}

impl AudioPlayback {
    fn init(channels: u16) -> Result<Self, AudioPlaybackInitError> {
        let host = cpal::default_host();
        let device =
            host.default_output_device().ok_or(AudioPlaybackInitError)?;
        let config = device
            .supported_output_configs()
            .map_err(|_| AudioPlaybackInitError)?
            .filter(|x| x.channels() == channels)
            .next()
            .ok_or(AudioPlaybackInitError)?
            // .with_sample_rate(SampleRate(sample_rate))
            .with_max_sample_rate()
            .config();
        Ok(Self { device, config })
    }
}

#[derive(StructOpt)]
enum Opt {
    Server,
    Client { addr: IpAddr },
}

fn main() {
    let opt = Opt::from_args();
    match opt {
        Opt::Server => {
            if let Err(e) = server() {
                eprintln!("Server error: {}", e);
            }
        }
        Opt::Client { addr } => {
            if let Err(e) = client(addr) {
                eprintln!("Client error: {}", e);
            }
        }
    }
}

const PORT: u16 = 5134;

#[derive(Decode, Encode)]
enum Packet {
    Henlo(String, Format),
    Data(Vec<f32>),
}

fn server() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("!!! Warning !!!");
    eprintln!("Do not connect from same computer as server");
    eprintln!("");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, PORT))?;
    let audio_buffer =
        Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(48000)));

    let channels = 2;
    let playback = AudioPlayback::init(channels)?;
    eprintln!("Audio playback initialized");

    let audio_buffer2 = Arc::clone(&audio_buffer);
    let stream = playback.device.build_output_stream(
        &playback.config,
        move |data: &mut [f32], _| {
            let mut samples = audio_buffer2.lock().unwrap();
            for d in data.chunks_exact_mut(channels as usize) {
                for d in d {
                    *d = samples.pop_back().unwrap_or_default();
                }
            }
        },
        |err| {
            eprintln!("{:?}", err);
        },
    )?;
    stream.play()?;
    eprintln!("Audio playback started");

    let config = Configuration::standard();
    'main: for stream in listener.incoming() {
        let mut stream = stream?;
        let packet = bincode::decode_from_std_read(&mut stream, config)?;
        if let Packet::Henlo(name, _) = packet {
            eprintln!("Client connected: {}", name);
            let mut stream = snap::read::FrameDecoder::new(stream);
            loop {
                let packet =
                    match bincode::decode_from_std_read(&mut stream, config) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("Data read error: {}", e);
                            eprintln!("Back to listening");
                            continue 'main;
                        }
                    };
                if let Packet::Data(data) = packet {
                    audio_buffer.lock().unwrap().extend(data);
                }
            }
        }
    }
    Ok(())
}

fn client(addr: IpAddr) -> Result<(), Box<dyn std::error::Error>> {
    let buffer_duration = Duration::from_millis(100);
    let mut audio_capture = AudioCapture::init(buffer_duration).unwrap();
    eprintln!("Audio capture initialized");
    let format = audio_capture.format().unwrap();
    println!("{:#?}", format);

    if !matches!(format.sample_format, SampleFormat::Float32) {
        todo!("sample formats different than f32");
    }

    let actual_duration = Duration::from_secs_f32(
        buffer_duration.as_secs_f32() * audio_capture.buffer_frame_size as f32
            / format.sample_rate as f32
            / 1000.,
    ) / 2;

    'main: loop {
        let _ = audio_capture.stop();
        let mut stream = loop {
            match TcpStream::connect((addr, PORT)) {
                Ok(s) => break s,
                Err(e) => eprintln!("connecton error: {}", e),
            }
            std::thread::sleep(Duration::from_secs(10));
        };
        eprintln!(
            "Socket bound at port {}",
            stream.local_addr().unwrap().port()
        );

        let config = Configuration::standard();
        bincode::encode_into_std_write(
            Packet::Henlo("client 0.1".into(), format),
            &mut stream,
            config,
        )?;
        eprintln!("henlo sent");

        audio_capture.start().unwrap();
        eprintln!("Audio capture started");

        let mut iter = (0u64..)
            .map(|i| (i as f32 * 2000. / format.sample_rate as f32).sin());

        let mut stream = snap::write::FrameEncoder::new(stream);
        loop {
            std::thread::sleep(actual_duration);
            let res = audio_capture.read_samples(|data, _| {
                let data = (&mut iter).take(data.len()).collect::<Vec<_>>();
                bincode::encode_into_std_write(
                    Packet::Data(data.to_vec()),
                    &mut stream,
                    config,
                )?;
                Ok::<_, EncodeError>(())
            });
            if let Err(e) = res {
                eprintln!("Error: {:?}", e);
                continue 'main;
            }
        }
    }
}
