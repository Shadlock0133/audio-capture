use std::{mem::size_of, ptr::null_mut, time::Duration};

use winapi::{
    shared::{
        mmreg::{
            WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_EXTENSIBLE,
            WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM,
        },
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

use crate::{Format, SampleFormat, read_unaligned, win::common::{DATAFORMAT_SUBTYPE_IEEE_FLOAT, DATAFORMAT_SUBTYPE_PCM}};

use super::common::{WinError, winapi_result};

pub struct AudioCapture {
    pub buffer_frame_size: u32,
    pub wave_format: *mut WAVEFORMATEX,
    pub channels: u16,
    pub enumerator: *mut IMMDeviceEnumerator,
    pub device: *mut IMMDevice,
    pub client: *mut IAudioClient,
    pub capture_client: *mut IAudioCaptureClient,
}

impl AudioCapture {
    pub fn init(buffer_duration: Duration) -> Result<Self, WinError> {
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

    pub fn read_samples<F>(&mut self, mut f: F) -> Result<(), WinError>
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
pub struct Info {
    pub is_silent: bool,
    pub data_discontinuity: bool,
    pub timestamp_error: bool,
}

#[derive(Debug)]
pub struct UnknownFormat;
