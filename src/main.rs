mod win;

use std::{
    collections::VecDeque,
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};

use bincode::{config::Configuration, Decode, Encode};
use structopt::StructOpt;
use win::capture::*;

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
    // hnlo
    Henlo(String, Format),
    // data
    Data(Vec<f32>),
}

fn read_packet_from(
    socket: &mut UdpSocket,
) -> io::Result<(Packet, SocketAddr)> {
    let mut buf = [0; 4];
    let (len, addr) = socket.peek_from(&mut buf)?;
    if len != 4 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let size = u32::from_le_bytes(buf);
    let mut data = vec![0; size as usize + 4];
    socket.recv_from(&mut data)?;
    let config = Configuration::standard();
    let packet = bincode::decode_from_slice(&data[4..], config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((packet, addr))
}

fn read_packet(socket: &mut UdpSocket) -> io::Result<Packet> {
    let mut buf = [0; 4];
    let len = socket.peek(&mut buf)?;
    if len != 4 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let size = u32::from_le_bytes(buf);
    let mut data = vec![0; size as usize + 4];
    socket.recv(&mut data)?;
    let config = Configuration::standard();
    let packet = bincode::decode_from_slice(&data[4..], config)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    Ok(packet)
}

fn server() -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, PORT))?;
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

    loop {
        let (packet, addr) = read_packet_from(&mut socket)?;
        if let Packet::Henlo(name, _) = packet {
            eprintln!("Client connected: {}", name);
            socket.connect(addr)?;
            loop {
                if let Packet::Data(data) = read_packet(&mut socket)? {
                    audio_buffer.lock().unwrap().extend(data);
                }
            }
        }
    }
}

fn send_packet(socket: &mut UdpSocket, packet: Packet) -> io::Result<()> {
    let config = Configuration::standard();
    let buf = bincode::encode_to_vec(packet, config)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    socket.send(&buf)?;
    Ok(())
}

fn client(addr: IpAddr) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    eprintln!(
        "Socket bound at port {}",
        socket.local_addr().unwrap().port()
    );
    eprintln!("Attempting connection to {}", addr);
    socket.connect((addr, PORT))?;

    let buffer_duration = Duration::from_millis(100);
    let mut audio_capture = AudioCapture::init(buffer_duration).unwrap();
    eprintln!("Audio capture initialized");
    let format = audio_capture.format().unwrap();
    println!("{:#?}", format);

    if !matches!(format.sample_format, SampleFormat::Float32) {
        todo!("sample formats different than f32");
    }

    send_packet(&mut socket, Packet::Henlo("client 0.1".into(), format))?;
    eprintln!("henlo sent");

    let actual_duration = Duration::from_secs_f32(
        buffer_duration.as_secs_f32() * audio_capture.buffer_frame_size as f32
            / format.sample_rate as f32
            / 1000.,
    ) / 2;

    audio_capture.start().unwrap();
    eprintln!("Audio capture started");

    loop {
        std::thread::sleep(actual_duration);
        audio_capture
            .read_samples(|data, _| {
                send_packet(&mut socket, Packet::Data(data.to_vec())).unwrap();
            })
            .unwrap();
    }
}

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

    pub fn to_hound(self) -> hound::SampleFormat {
        match self {
            SampleFormat::Int8 => hound::SampleFormat::Int,
            SampleFormat::Int16 => hound::SampleFormat::Int,
            SampleFormat::Float32 => hound::SampleFormat::Float,
        }
    }
}
