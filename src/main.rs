use std::{fmt, mem::size_of, ptr::null_mut, time::Duration};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use winapi::{Class, Interface, shared::{
        guiddef,
        ksmedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, KSDATAFORMAT_SUBTYPE_PCM},
        mmreg::{
            WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_EXTENSIBLE,
            WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM,
        },
        winerror::S_OK,
    }, um::{audioclient::{
            IAudioCaptureClient, IAudioClient,
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
        }, audiosessiontypes::{
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
        }, combaseapi::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, CoUninitialize}, mmdeviceapi::{
            eConsole, eRender, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        }, objbase::CoInitialize, winbase::{
            FormatMessageA, LocalFree, FORMAT_MESSAGE_ALLOCATE_BUFFER,
            FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
        }}};

macro_rules! read_unaligned {
    ($v:ident $(. $field:ident)*) => {
        std::ptr::addr_of!((*$v) $(.$field)* ).read_unaligned()
    };
}

fn _playback() {
    let host = cpal::default_host();
    let device = host.default_output_device().unwrap();
    let config = device
        .supported_output_configs()
        .unwrap()
        .filter(|x| x.channels() == 2)
        .next()
        .unwrap()
        // .with_sample_rate(SampleRate(sample_rate))
        .with_max_sample_rate()
        .config();

    let sample_rate = config.sample_rate.0;

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
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [i16], _| {
                for d in data.chunks_exact_mut(2) {
                    let sample = iter.next().unwrap_or_default();
                    d[0] = sample;
                    d[1] = sample;
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

struct WinError(i32);

impl fmt::Debug for WinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WinError")
            .field(&error_to_string(self.0))
            .finish()
    }
}

fn winapi_result(hresult: i32) -> Result<(), WinError> {
    if hresult == S_OK {
        Ok(())
    } else {
        Err(WinError(hresult))
    }
}

struct AudioCapture {
    buffer_frame_size: u32,
    wave_format: *mut WAVEFORMATEX,
    channels: u16,
    enumerator: *mut IMMDeviceEnumerator,
    device: *mut IMMDevice,
    client: *mut IAudioClient,
    capture_client: *mut IAudioCaptureClient,
}

impl AudioCapture {
    fn init(buffer_duration: Duration) -> Result<Self, WinError> {
        winapi_result(unsafe { CoInitialize(null_mut()) })?;

        let mut enumerator: *mut IMMDeviceEnumerator = null_mut();
        winapi_result(unsafe {
            CoCreateInstance(
                &MMDeviceEnumerator::uuidof(),
                null_mut(),
                CLSCTX_ALL,
                &IMMDeviceEnumerator::uuidof(),
                &mut enumerator as *mut _ as _,
            )
        })?;

        let mut device: *mut IMMDevice = null_mut();
        winapi_result(unsafe {
            (&*enumerator).GetDefaultAudioEndpoint(
                eRender,
                eConsole,
                &mut device,
            )
        })?;

        let mut client: *mut IAudioClient = null_mut();
        winapi_result(unsafe {
            (&*device).Activate(
                &IAudioClient::uuidof(),
                CLSCTX_ALL,
                null_mut(),
                &mut client as *mut _ as _,
            )
        })?;

        winapi_result(unsafe {
            (&*device).Activate(
                &IAudioClient::uuidof(),
                CLSCTX_ALL,
                null_mut(),
                &mut client as *mut _ as _,
            )
        })?;

        let mut wave_format: *mut WAVEFORMATEX = null_mut();
        winapi_result(unsafe { (&*client).GetMixFormat(&mut wave_format) })
            .unwrap();

        let channels = unsafe { read_unaligned!(wave_format.nChannels) };

        // 100ns unit
        let dur = buffer_duration.as_secs() as i64 * 100_000_000_000
            + buffer_duration.subsec_nanos() as i64 * 100;
        winapi_result(unsafe {
            (&*client).Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                dur,
                0,
                wave_format,
                null_mut(),
            )
        })
        .unwrap();

        let mut buffer_frame_size = 0;
        winapi_result(unsafe {
            (&*client).GetBufferSize(&mut buffer_frame_size)
        })
        .unwrap();

        let mut capture_client: *mut IAudioCaptureClient = null_mut();
        winapi_result(unsafe {
            (&*client).GetService(
                &IAudioCaptureClient::uuidof(),
                &mut capture_client as *mut _ as _,
            )
        })
        .unwrap();

        Ok(Self {
            buffer_frame_size,
            wave_format,
            channels,
            enumerator,
            device,
            client,
            capture_client,
        })
    }

    fn format(&self) -> Result<Format, UnknownFormat> {
        let wave_format = self.wave_format;

        let channels;
        let sample_rate;
        let sample_format;
        unsafe {
            let sample_bitsize = read_unaligned!(wave_format.wBitsPerSample);
            let struct_size = read_unaligned!(wave_format.cbSize);
            let format_tag = read_unaligned!(wave_format.wFormatTag);
            sample_format = match (format_tag, sample_bitsize) {
                (WAVE_FORMAT_PCM, 8) => Some(SampleFormat::Int8),
                (WAVE_FORMAT_PCM, 16) => Some(SampleFormat::Int16),
                (WAVE_FORMAT_IEEE_FLOAT, 32) => Some(SampleFormat::Float32),
                (WAVE_FORMAT_EXTENSIBLE, _)
                    if size_of::<WAVEFORMATEXTENSIBLE>()
                        - size_of::<WAVEFORMATEX>()
                        == struct_size as usize =>
                {
                    let wave_format: *mut WAVEFORMATEXTENSIBLE =
                        wave_format as _;
                    let format_guid = read_unaligned!(wave_format.SubFormat);
                    match (format_guid.into(), sample_bitsize) {
                        (DATAFORMAT_SUBTYPE_PCM, 8) => Some(SampleFormat::Int8),
                        (DATAFORMAT_SUBTYPE_PCM, 16) => {
                            Some(SampleFormat::Int16)
                        }
                        (DATAFORMAT_SUBTYPE_IEEE_FLOAT, 32) => {
                            Some(SampleFormat::Float32)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            sample_rate = read_unaligned!(wave_format.nSamplesPerSec);
            channels = read_unaligned!(wave_format.nChannels);
        }
        let sample_format = sample_format.ok_or(UnknownFormat)?;

        Ok(Format {
            channels,
            sample_rate,
            sample_format,
        })
    }

    fn start(&mut self) -> Result<(), WinError> {
        winapi_result(unsafe { (*self.client).Start() })
    }

    fn stop(&mut self) -> Result<(), WinError> {
        winapi_result(unsafe { (*self.client).Stop() })
    }

    fn read_samples<F>(&mut self, mut f: F) -> Result<(), WinError>
    where
        F: FnMut(&[f32], Info),
    {
        let mut packet_length = 0;
        winapi_result(unsafe {
            (*self.capture_client).GetNextPacketSize(&mut packet_length)
        })?;

        while packet_length > 0 {
            let mut buffer: *mut u8 = null_mut();
            let mut buffer_size = 0;
            let mut flags = 0;
            winapi_result(unsafe {
                (*self.capture_client).GetBuffer(
                    &mut buffer,
                    &mut buffer_size,
                    &mut flags,
                    null_mut(),
                    null_mut(),
                )
            })?;

            let is_silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT) != 0;
            let data_discontinuity =
                (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) != 0;
            let timestamp_error =
                (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR) != 0;

            let data = unsafe {
                std::slice::from_raw_parts(
                    buffer as *mut f32,
                    buffer_size as usize * self.channels as usize,
                )
            };

            let info = Info {
                is_silent,
                data_discontinuity,
                timestamp_error,
            };

            f(data, info);

            winapi_result(unsafe {
                (*self.capture_client).ReleaseBuffer(buffer_size)
            })?;

            winapi_result(unsafe {
                (*self.capture_client).GetNextPacketSize(&mut packet_length)
            })?;
        }
        Ok(())
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(self.wave_format as _);
            (*self.capture_client).Release();
            (*self.client).Release();
            (*self.device).Release();
            (*self.enumerator).Release();
            CoUninitialize();
        }
    }
}

#[allow(unused)]
struct Info {
    pub is_silent: bool,
    pub data_discontinuity: bool,
    pub timestamp_error: bool,
}

#[derive(Debug)]
struct UnknownFormat;

struct Format {
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: SampleFormat,
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
enum SampleFormat {
    Int8,
    Int16,
    Float32,
}

impl SampleFormat {
    fn bits_per_sample(self) -> u16 {
        match self {
            SampleFormat::Int8 => 8,
            SampleFormat::Int16 => 16,
            SampleFormat::Float32 => 32,
        }
    }

    fn to_hound(self) -> hound::SampleFormat {
        match self {
            SampleFormat::Int8 => hound::SampleFormat::Int,
            SampleFormat::Int16 => hound::SampleFormat::Int,
            SampleFormat::Float32 => hound::SampleFormat::Float,
        }
    }
}

#[derive(PartialEq, Eq)]
struct Guid(u32, u16, u16, [u8; 8]);

impl Guid {
    const fn from_winapi(guid: guiddef::GUID) -> Self {
        Self(guid.Data1, guid.Data2, guid.Data3, guid.Data4)
    }
}

impl From<guiddef::GUID> for Guid {
    fn from(guid: guiddef::GUID) -> Self {
        Self::from_winapi(guid)
    }
}

const _AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x80000000;
const _AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY: u32 = 0x08000000;

const DATAFORMAT_SUBTYPE_PCM: Guid =
    Guid::from_winapi(KSDATAFORMAT_SUBTYPE_PCM);
const DATAFORMAT_SUBTYPE_IEEE_FLOAT: Guid =
    Guid::from_winapi(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);

fn error_to_string(code: i32) -> String {
    let mut buffer: *mut i8 = null_mut();
    unsafe {
        let size = FormatMessageA(
            FORMAT_MESSAGE_ALLOCATE_BUFFER
                | FORMAT_MESSAGE_FROM_SYSTEM
                | FORMAT_MESSAGE_IGNORE_INSERTS,
            null_mut(),
            code as u32,
            0,
            &mut buffer as *mut _ as *mut i8,
            0,
            null_mut(),
        );
        let slice = std::slice::from_raw_parts(buffer as _, size as usize);
        let str = std::str::from_utf8(slice).unwrap();
        let string = str.to_string();
        LocalFree(buffer as _);
        string
    }
}
