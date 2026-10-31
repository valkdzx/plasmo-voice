//! The microphone and the speakers, through cpal. A cpal stream may not leave the thread that
//! made it, so each lives on a thread of its own until its handle is dropped.
use crate::mixer::Mixer;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SizedSample;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// An open stream: dropping it closes the stream and ends its thread.
pub struct Stream {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    /// The device's name and sample rate.
    pub name: String,
    pub rate: u32,
}

impl Drop for Stream {
    /// The thread closes its stream and ends on its own. It isn't joined: at process exit the
    /// system has ended it already, and joining it then panics.
    fn drop(&mut self) {
        self.stop.take();
        self.thread.take();
    }
}

pub fn input_devices() -> Vec<String> {
    names(cpal::default_host().input_devices().ok())
}

pub fn output_devices() -> Vec<String> {
    names(cpal::default_host().output_devices().ok())
}

fn names(devices: Option<impl Iterator<Item = cpal::Device>>) -> Vec<String> {
    let mut out = Vec::new();
    for d in devices.into_iter().flatten() {
        if let Ok(name) = d.name()
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// The device named `wanted` (the system's default when it is None or gone).
fn device(input: bool, wanted: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if let Some(wanted) = wanted {
        let devices = if input { host.input_devices() } else { host.output_devices() };
        if let Some(d) = devices.ok().into_iter().flatten().find(|d| d.name().is_ok_and(|n| n == wanted)) {
            return Ok(d);
        }
    }
    let d = if input { host.default_input_device() } else { host.default_output_device() };
    d.ok_or_else(|| if input { "no microphone".to_string() } else { "no audio output".to_string() })
}

/// Run `open` on a thread of its own and keep what it opens there.
fn spawn(
    name: &str,
    open: impl FnOnce() -> Result<(cpal::Stream, String, u32), String> + Send + 'static,
) -> Result<Stream, String> {
    let (ready, ready_rx) = mpsc::channel();
    let (stop, stop_rx): (Sender<()>, Receiver<()>) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || match open() {
            Ok((stream, name, rate)) => {
                let _ = ready.send(Ok((name, rate)));
                // Until the handle drops its sender.
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(e) => {
                let _ = ready.send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv() {
        Ok(Ok((name, rate))) => Ok(Stream { stop: Some(stop), thread: Some(thread), name, rate }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("the audio thread stopped".into()),
    }
}

/// The microphone: blocks of mono samples at its own rate go to `sink`.
pub fn capture(wanted: Option<String>, sink: SyncSender<Vec<f32>>) -> Result<Stream, String> {
    spawn("proximity-voice-mic", move || {
        let device = device(true, wanted.as_deref())?;
        let name = device.name().unwrap_or_default();
        let config = device.default_input_config().map_err(|e| e.to_string())?;
        use cpal::SampleFormat as F;
        let stream = match config.sample_format() {
            F::F32 => input::<f32>(&device, &config, sink),
            F::F64 => input::<f64>(&device, &config, sink),
            F::I16 => input::<i16>(&device, &config, sink),
            F::I32 => input::<i32>(&device, &config, sink),
            F::I8 => input::<i8>(&device, &config, sink),
            F::U16 => input::<u16>(&device, &config, sink),
            F::U32 => input::<u32>(&device, &config, sink),
            F::U8 => input::<u8>(&device, &config, sink),
            other => Err(format!("unsupported microphone format {other}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, name, config.sample_rate().0))
    })
}

fn input<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    sink: SyncSender<Vec<f32>>,
) -> Result<cpal::Stream, String>
where
    f32: cpal::FromSample<T>,
{
    let channels = config.channels().max(1) as usize;
    device
        .build_input_stream(
            &config.config(),
            move |data: &[T], _| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32)
                    .collect();
                // A full queue (the encoder is stuck) drops the block rather than block the device.
                let _ = sink.try_send(mono);
            },
            |e| eprintln!("Proximity Voice: microphone error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// The speakers, playing `mixer`.
pub fn playback(wanted: Option<String>, mixer: Arc<Mutex<Mixer>>) -> Result<Stream, String> {
    spawn("proximity-voice-out", move || {
        let device = device(false, wanted.as_deref())?;
        let name = device.name().unwrap_or_default();
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        use cpal::SampleFormat as F;
        let stream = match config.sample_format() {
            F::F32 => output::<f32>(&device, &config, mixer),
            F::F64 => output::<f64>(&device, &config, mixer),
            F::I16 => output::<i16>(&device, &config, mixer),
            F::I32 => output::<i32>(&device, &config, mixer),
            F::I8 => output::<i8>(&device, &config, mixer),
            F::U16 => output::<u16>(&device, &config, mixer),
            F::U32 => output::<u32>(&device, &config, mixer),
            F::U8 => output::<u8>(&device, &config, mixer),
            other => Err(format!("unsupported output format {other}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok((stream, name, config.sample_rate().0))
    })
}

fn output<T: SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    mixer: Arc<Mutex<Mixer>>,
) -> Result<cpal::Stream, String> {
    let rate = config.sample_rate().0;
    let channels = config.channels().max(1) as usize;
    let mut scratch = Vec::new();
    device
        .build_output_stream(
            &config.config(),
            move |data: &mut [T], _| {
                scratch.resize(data.len(), 0.0f32);
                match mixer.lock() {
                    Ok(mut m) => m.render(&mut scratch, channels, rate),
                    Err(_) => scratch.fill(0.0),
                }
                for (out, s) in data.iter_mut().zip(&scratch) {
                    *out = T::from_sample(*s);
                }
            },
            |e| eprintln!("Proximity Voice: audio output error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}
