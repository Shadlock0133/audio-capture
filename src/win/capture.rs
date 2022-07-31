use std::{fmt, mem::size_of, ptr::null_mut, time::Duration};

use winapi::{
    shared::mmreg::{
        WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_EXTENSIBLE,
        WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM,
    },
    um::{
        audioclient::{
            IAudioCaptureClient, IAudioClient,
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR,
        },
        audiosessiontypes::{
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
        },
        combaseapi::{
            CoCreateInstance, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        },
        mmdeviceapi::{
            eConsole, eRender, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        },
        objbase::CoInitialize,
    },
    Class, Interface,
};

use crate::{
    read_unaligned,
    win::common::{DATAFORMAT_SUBTYPE_IEEE_FLOAT, DATAFORMAT_SUBTYPE_PCM},
    Format, SampleFormat,
};

use super::common::{winapi_result, WinError};

pub struct AudioCapture {
    pub buffer_frame_size: u32,
    pub wave_format: *mut WAVEFORMATEX,
    pub channels: u16,
    pub enumerator: *mut IMMDeviceEnumerator,
    pub device: *mut IMMDevice,
    pub client: *mut IAudioClient,
    pub capture_client: *mut IAudioCaptureClient,
    // other library might have run CoInitialize already
    should_run_couninitalize_on_drop: bool,
}

impl AudioCapture {
    pub fn init(buffer_duration: Duration) -> Result<Self, WinError> {
        let should_run_couninitilize_on_drop =
            winapi_result(unsafe { CoInitialize(null_mut()) }).is_ok();

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

        let mut wave_format: *mut WAVEFORMATEX = null_mut();
        winapi_result(unsafe { (&*client).GetMixFormat(&mut wave_format) })
            .unwrap();

        let channels = unsafe { read_unaligned!(wave_format.nChannels) };

        // 100ns unit
        let dur = (buffer_duration.as_secs() as i64)
            .checked_mul(100_000_000_000)
            .expect("duration math overflow")
            .checked_add(buffer_duration.subsec_nanos() as i64 * 100)
            .expect("duration math overflow");
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
            should_run_couninitalize_on_drop: should_run_couninitilize_on_drop,
        })
    }

    pub fn format(&self) -> Result<Format, UnknownFormat> {
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

    pub fn start(&mut self) -> Result<(), WinError> {
        winapi_result(unsafe { (*self.client).Start() })
    }

    pub fn stop(&mut self) -> Result<(), WinError> {
        winapi_result(unsafe { (*self.client).Stop() })
    }

    /// Reads samples from system's internal queue, running provided callback
    /// for each "packet", then return.
    /// 
    /// You will need to call this function in loop to keep reading new samples,
    /// as it doesn't spawn background thread for you. It's done this way to
    /// be more flexible for users.
    pub fn read_samples<E, F>(
        &mut self,
        mut f: F,
    ) -> Result<(), ReadSamplesError<E>>
    where
        F: FnMut(&[f32], Info) -> Result<(), E>,
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

            let r = f(data, info).map_err(|e| ReadSamplesError::E(e));

            winapi_result(unsafe {
                (*self.capture_client).ReleaseBuffer(buffer_size)
            })?;

            r?;

            winapi_result(unsafe {
                (*self.capture_client).GetNextPacketSize(&mut packet_length)
            })?;
        }
        Ok(())
    }
}

pub enum ReadSamplesError<E> {
    E(E),
    WinError(WinError),
}

impl<E: fmt::Debug> fmt::Debug for ReadSamplesError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::E(e) => e.fmt(f),
            Self::WinError(e) => e.fmt(f),
        }
    }
}

impl<E: fmt::Display> fmt::Display for ReadSamplesError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::E(e) => e.fmt(f),
            Self::WinError(e) => e.fmt(f),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ReadSamplesError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadSamplesError::E(e) => Some(e),
            ReadSamplesError::WinError(e) => Some(e),
        }
    }
}

impl<E> From<WinError> for ReadSamplesError<E> {
    fn from(e: WinError) -> Self {
        Self::WinError(e)
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

            if self.should_run_couninitalize_on_drop {
                CoUninitialize();
            }
        }
    }
}

#[allow(unused)]
pub struct Info {
    pub is_silent: bool,
    pub data_discontinuity: bool,
    pub timestamp_error: bool,
}

#[derive(Debug)]
pub struct UnknownFormat;

impl fmt::Display for UnknownFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for UnknownFormat {}
