//! High-level HDA driver API.
//!
//! Ties together controller register access, codec communication,
//! widget discovery, and stream management into a usable interface.

use alloc::vec::Vec;
use log::{debug, error, info, trace, warn};

use crate::codec::*;
use crate::registers::*;
use crate::stream::*;
use crate::widget::*;

// =============================================================================
// HDA PCI identification
// =============================================================================

/// PCI class code for multimedia audio controller.
pub const PCI_CLASS_MULTIMEDIA_AUDIO: u8 = 0x04;
/// PCI subclass for HDA.
pub const PCI_SUBCLASS_HDA: u8 = 0x03;

// =============================================================================
// Controller state
// =============================================================================

/// High-level Intel HDA controller.
pub struct HdaController {
    /// BAR0 MMIO base address.
    bar0: usize,
    /// CORB/RIRB communication channel.
    corb_rirb: CorbRirb,
    /// Discovered codecs with their widget trees.
    codecs: Vec<Codec>,
    /// Number of input stream descriptors.
    num_iss: usize,
    /// Number of output stream descriptors.
    num_oss: usize,
    /// Number of bidirectional stream descriptors.
    num_bss: usize,
    /// Active output (if configured).
    output: Option<OutputPath>,
}

/// A configured audio output path through the codec.
struct OutputPath {
    /// Codec address.
    codec_addr: u8,
    /// DAC widget NID.
    dac_nid: u8,
    /// Pin widget NID.
    pin_nid: u8,
    /// Intermediate widget NIDs (mixers, selectors).
    intermediate_nids: Vec<u8>,
    /// The active stream.
    stream: Option<HdaStream>,
}

/// A handle for controlling audio output.
pub struct HdaOutput<'a> {
    controller: &'a mut HdaController,
}

impl HdaController {
    /// Initialize the HDA controller at the given BAR0 MMIO address.
    ///
    /// This performs a full initialization sequence:
    /// 1. Read controller capabilities
    /// 2. Reset the controller
    /// 3. Initialize CORB/RIRB
    /// 4. Discover codecs and enumerate widgets
    /// 5. Find an audio output path
    ///
    /// # Safety
    /// `pci_bar0_addr` must be a valid, memory-mapped BAR0 address for an
    /// Intel HDA controller.
    pub unsafe fn init(pci_bar0_addr: usize) -> Result<Self, &'static str> {
        info!("hda: initializing controller at BAR0=0x{:016x}", pci_bar0_addr);
        let bar0 = pci_bar0_addr;

        // --- Read version ---
        let vmaj = unsafe { read8(bar0, VMAJ) };
        let vmin = unsafe { read8(bar0, VMIN) };
        info!("hda: controller version {}.{}", vmaj, vmin);
        if vmaj != 1 {
            error!("hda: unsupported HDA version {}.{} (expected 1.x)", vmaj, vmin);
            return Err("unsupported HDA version");
        }

        // --- Read global capabilities ---
        let gcap = unsafe { read16(bar0, GCAP) };
        let supports_64bit = gcap & GCAP_64OK != 0;
        let nsdo = ((gcap >> GCAP_NSDO_SHIFT) & GCAP_NSDO_MASK) as usize;
        let num_bss = ((gcap >> GCAP_BSS_SHIFT) & GCAP_BSS_MASK) as usize;
        let num_iss = ((gcap >> GCAP_ISS_SHIFT) & GCAP_ISS_MASK) as usize;
        let num_oss = ((gcap >> GCAP_OSS_SHIFT) & GCAP_OSS_MASK) as usize;

        info!(
            "hda: GCAP=0x{:04x}: 64bit={}, NSDO={}, BSS={}, ISS={}, OSS={}",
            gcap, supports_64bit, nsdo, num_bss, num_iss, num_oss
        );

        if num_oss == 0 {
            error!("hda: controller has no output stream descriptors");
            return Err("no output streams");
        }

        // --- Reset controller ---
        unsafe {
            Self::reset_controller(bar0)?;
        }

        // --- Initialize CORB/RIRB ---
        let mut corb_rirb = unsafe { CorbRirb::init(bar0) };

        // --- Enable interrupts ---
        unsafe {
            // Enable controller interrupt and global interrupt
            write32(bar0, INTCTL, INTCTL_GIE | INTCTL_CIE);
            debug!("hda: interrupts enabled (GIE + CIE)");
        }

        // --- Discover codecs ---
        let codecs = unsafe { discover_codecs(&mut corb_rirb, bar0) };
        if codecs.is_empty() {
            warn!("hda: no codecs found on the link");
            return Err("no codecs found");
        }

        // --- Find output path ---
        let mut output = None;
        for codec in &codecs {
            let path = find_output_path(codec);
            if !path.is_empty() {
                // Extract DAC and pin from the path
                let dac_entry = path
                    .iter()
                    .find(|(_, t)| *t == WidgetType::AudioOutput);
                let pin_entry = path
                    .iter()
                    .find(|(_, t)| *t == WidgetType::PinComplex);

                if let (Some(&(dac_nid, _)), Some(&(pin_nid, _))) = (dac_entry, pin_entry) {
                    let intermediate_nids: Vec<u8> = path
                        .iter()
                        .filter(|(_, t)| {
                            *t != WidgetType::AudioOutput && *t != WidgetType::PinComplex
                        })
                        .map(|(nid, _)| *nid)
                        .collect();

                    info!(
                        "hda: output path: codec={}, DAC=NID{}, pin=NID{}, intermediates={:?}",
                        codec.address, dac_nid, pin_nid, intermediate_nids
                    );

                    output = Some(OutputPath {
                        codec_addr: codec.address,
                        dac_nid,
                        pin_nid,
                        intermediate_nids,
                        stream: None,
                    });
                    break;
                }
            }
        }

        if output.is_none() {
            warn!("hda: no usable audio output path found (controller still usable)");
        }

        info!("hda: controller initialization complete");

        Ok(HdaController {
            bar0,
            corb_rirb,
            codecs,
            num_iss,
            num_oss,
            num_bss,
            output,
        })
    }

    /// Reset the HDA controller (CRST bit in GCTL).
    ///
    /// # Safety
    /// BAR0 must be valid.
    unsafe fn reset_controller(bar0: usize) -> Result<(), &'static str> {
        info!("hda: resetting controller...");

        // Enter reset: clear CRST
        unsafe {
            let gctl = read32(bar0, GCTL);
            write32(bar0, GCTL, gctl & !GCTL_CRST);
        }

        // Wait for CRST to read 0 (controller in reset)
        let mut attempts = 0u32;
        loop {
            let gctl = unsafe { read32(bar0, GCTL) };
            if gctl & GCTL_CRST == 0 {
                break;
            }
            attempts += 1;
            if attempts > 100_000 {
                error!("hda: controller reset enter timeout");
                return Err("reset enter timeout");
            }
            core::hint::spin_loop();
        }
        debug!("hda: controller entered reset after {} iterations", attempts);

        // Wait a bit for codec link to settle (spec says >= 100us, we spin)
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }

        // Exit reset: set CRST
        unsafe {
            let gctl = read32(bar0, GCTL);
            write32(bar0, GCTL, gctl | GCTL_CRST);
        }

        // Wait for CRST to read 1 (controller out of reset)
        attempts = 0;
        loop {
            let gctl = unsafe { read32(bar0, GCTL) };
            if gctl & GCTL_CRST != 0 {
                break;
            }
            attempts += 1;
            if attempts > 100_000 {
                error!("hda: controller reset exit timeout");
                return Err("reset exit timeout");
            }
            core::hint::spin_loop();
        }
        debug!("hda: controller exited reset after {} iterations", attempts);

        // Wait for codecs to initialize (spec: at least 521us after CRST=1,
        // or 25 frames at 48kHz = ~521us). We spin generously.
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }

        // Check STATESTS for codec presence
        let statests = unsafe { read16(bar0, STATESTS) };
        info!(
            "hda: post-reset STATESTS=0x{:04x} ({} codec(s) detected)",
            statests,
            statests.count_ones()
        );

        // Enable unsolicited responses
        unsafe {
            let gctl = read32(bar0, GCTL);
            write32(bar0, GCTL, gctl | GCTL_UNSOL);
        }
        debug!("hda: unsolicited responses enabled");

        Ok(())
    }

    /// Get the number of discovered codecs.
    pub fn codec_count(&self) -> usize {
        self.codecs.len()
    }

    /// Get a reference to the discovered codecs.
    pub fn codecs(&self) -> &[Codec] {
        &self.codecs
    }

    /// Check if an audio output path was found.
    pub fn has_output(&self) -> bool {
        self.output.is_some()
    }

    /// Get a mutable handle for audio output operations.
    pub fn output(&mut self) -> Option<HdaOutput<'_>> {
        if self.output.is_some() {
            Some(HdaOutput { controller: self })
        } else {
            None
        }
    }

    /// Read the wall clock counter (32-bit, increments at 24MHz).
    pub fn wall_clock(&self) -> u32 {
        unsafe { read32(self.bar0, WALCLK) }
    }
}

impl<'a> HdaOutput<'a> {
    /// Play PCM audio samples.
    ///
    /// `samples` are interleaved signed 16-bit PCM samples.
    /// `sample_rate` is the playback rate in Hz (e.g. 48000).
    /// `channels` is the number of audio channels (1=mono, 2=stereo).
    ///
    /// # Safety
    /// Controller must be fully initialized with a valid output path.
    pub unsafe fn play_pcm(
        &mut self,
        samples: &[i16],
        sample_rate: u32,
        channels: u16,
    ) -> Result<(), &'static str> {
        // Extract output path info and stop any existing stream first,
        // then drop the mutable borrow so we can use self freely below.
        let (codec_addr, dac_nid, pin_nid) = {
            let output = self
                .controller
                .output
                .as_mut()
                .ok_or("no output path")?;

            let codec_addr = output.codec_addr;
            let dac_nid = output.dac_nid;
            let pin_nid = output.pin_nid;

            // Stop any existing stream
            if let Some(ref mut stream) = output.stream {
                unsafe {
                    stream.stop();
                }
            }

            (codec_addr, dac_nid, pin_nid)
        };

        let bar0 = self.controller.bar0;

        info!(
            "hda: play_pcm: {} samples, {}Hz, {} channels",
            samples.len(),
            sample_rate,
            channels
        );

        // Determine format
        let format = match (sample_rate, channels) {
            (48000, 2) => StreamFormat::PCM_48K_16BIT_STEREO,
            (48000, 1) => StreamFormat::PCM_48K_16BIT_MONO,
            (44100, 2) => StreamFormat::PCM_44K1_16BIT_STEREO,
            (96000, 2) => StreamFormat::PCM_96K_16BIT_STEREO,
            _ => {
                // Build custom format
                let base_44k1 = sample_rate % 44100 == 0;
                let base = if base_44k1 { 44100 } else { 48000 };
                let mult = (sample_rate / base).saturating_sub(1) as u8;
                StreamFormat {
                    base_44k1,
                    rate_mult: mult.min(3),
                    rate_div: 0,
                    bits: 1, // 16-bit
                    channels_minus_1: channels.saturating_sub(1) as u8,
                }
            }
        };

        let fmt_val = format.encode();
        debug!("hda: stream format register value = 0x{:04x}", fmt_val);

        // Use the first output stream descriptor
        // Output streams start after input streams
        let sd_index = self.controller.num_iss;
        let stream_number: u8 = 1; // Stream numbers are 1-15

        // Create the stream
        let buffer_size = samples.len() * 2; // 16-bit samples = 2 bytes each
        let mut stream = unsafe {
            HdaStream::new(bar0, sd_index, stream_number, format, buffer_size)
        };

        // Fill the DMA buffer
        stream.fill_buffer(samples);

        // Configure the codec DAC: set stream/channel and format
        unsafe {
            // SET_CHANNEL/STREAM_ID: stream_number in bits 7:4, channel 0 in bits 3:0
            let stream_chan = ((stream_number as u16) << 4) | 0;
            self.controller.corb_rirb.send_verb_long(
                codec_addr,
                dac_nid,
                VERB_SET_CHANNEL_STREAMID,
                stream_chan,
            );
            debug!(
                "hda: DAC NID={}: stream/channel set to 0x{:04x}",
                dac_nid, stream_chan
            );

            // SET_STREAM_FORMAT on the DAC
            self.controller.corb_rirb.send_verb_long(
                codec_addr,
                dac_nid,
                VERB_SET_STREAM_FORMAT,
                fmt_val,
            );
            debug!(
                "hda: DAC NID={}: format set to 0x{:04x}",
                dac_nid, fmt_val
            );

            // Power on the DAC (D0)
            self.controller.corb_rirb.set_verb(
                codec_addr,
                dac_nid,
                VERB_SET_POWER_STATE,
                0x00,
            );
            trace!("hda: DAC NID={}: powered on (D0)", dac_nid);

            // Enable the output pin
            self.controller.corb_rirb.set_verb(
                codec_addr,
                pin_nid,
                VERB_SET_PIN_CTRL,
                PIN_CTRL_OUT_ENABLE | PIN_CTRL_HP_ENABLE,
            );
            debug!("hda: Pin NID={}: output + HP enabled", pin_nid);

            // Enable EAPD if the pin supports it
            let needs_eapd = self
                .controller
                .codecs
                .iter()
                .find(|c| c.address == codec_addr)
                .and_then(|c| c.widgets.iter().find(|w| w.nid == pin_nid))
                .map(|pw| pw.pin_caps & PIN_CAP_EAPD != 0)
                .unwrap_or(false);

            if needs_eapd {
                self.controller.corb_rirb.set_verb(
                    codec_addr,
                    pin_nid,
                    VERB_SET_EAPDBTL,
                    0x02, // EAPD enable
                );
                debug!("hda: Pin NID={}: EAPD enabled", pin_nid);
            }

            // Unmute and set gain on output amps along the path
            self.unmute_output_path(codec_addr, dac_nid, pin_nid);
        }

        // Start the stream
        unsafe {
            stream.start();
        }

        if let Some(ref mut output) = self.controller.output {
            output.stream = Some(stream);
        }
        info!("hda: PCM playback started");
        Ok(())
    }

    /// Stop audio playback.
    pub unsafe fn stop(&mut self) {
        if let Some(ref mut output) = self.controller.output {
            if let Some(ref mut stream) = output.stream {
                unsafe {
                    stream.stop();
                }
                info!("hda: playback stopped");
            }
        }
    }

    /// Set the output volume level (0-127, where 0 = minimum, 127 = maximum).
    ///
    /// # Safety
    /// Controller must be initialized with a valid output path.
    pub unsafe fn set_volume(&mut self, level: u8) {
        let output = match self.controller.output.as_ref() {
            Some(o) => o,
            None => {
                warn!("hda: set_volume called with no output path");
                return;
            }
        };

        let codec_addr = output.codec_addr;
        let dac_nid = output.dac_nid;
        let gain = level & 0x7F;

        info!(
            "hda: setting volume: codec={}, DAC NID={}, gain={}",
            codec_addr, dac_nid, gain
        );

        // Set output amp gain on the DAC (both channels, unmuted)
        let payload = AMP_SET_OUTPUT | AMP_SET_LEFT | AMP_SET_RIGHT | (gain as u16);
        unsafe {
            self.controller.corb_rirb.send_verb_long(
                codec_addr,
                dac_nid,
                VERB_SET_AMP_GAIN,
                payload,
            );
        }

        debug!("hda: volume set to {}/127", gain);
    }

    /// Play a simple beep/tone at the given frequency and duration.
    ///
    /// Generates a sine-wave tone and plays it through the output path.
    ///
    /// # Safety
    /// Controller must be initialized with a valid output path.
    pub unsafe fn beep(&mut self, frequency: u16, duration_ms: u32) -> Result<(), &'static str> {
        info!(
            "hda: generating beep: {}Hz for {}ms",
            frequency, duration_ms
        );

        let sample_rate = 48000u32;
        let channels = 2u16;
        let samples = generate_tone(frequency, sample_rate, channels, duration_ms);

        unsafe {
            self.play_pcm(&samples, sample_rate, channels)
        }
    }

    /// Unmute all output amplifiers along the output path.
    ///
    /// # Safety
    /// CORB/RIRB must be functional.
    unsafe fn unmute_output_path(&mut self, codec_addr: u8, dac_nid: u8, pin_nid: u8) {
        debug!(
            "hda: unmuting output path: codec={}, DAC={}, pin={}",
            codec_addr, dac_nid, pin_nid
        );

        let output = match self.controller.output.as_ref() {
            Some(o) => o,
            None => return,
        };

        // Collect NIDs to configure (clone to avoid borrow issues)
        let mut all_nids = Vec::new();
        all_nids.push(dac_nid);
        all_nids.extend_from_slice(&output.intermediate_nids);
        all_nids.push(pin_nid);

        // For each widget in the path, unmute output amp at max gain
        for &nid in &all_nids {
            // Find the widget to check capabilities
            let has_out_amp = self
                .controller
                .codecs
                .iter()
                .find(|c| c.address == codec_addr)
                .and_then(|c| c.widgets.iter().find(|w| w.nid == nid))
                .is_some_and(|w| w.caps & WIDGET_CAP_OUT_AMP != 0);

            if has_out_amp {
                // Set output amp: both channels, unmuted, max gain (0x7F)
                let payload =
                    AMP_SET_OUTPUT | AMP_SET_LEFT | AMP_SET_RIGHT | 0x7F;
                unsafe {
                    self.controller.corb_rirb.send_verb_long(
                        codec_addr,
                        nid,
                        VERB_SET_AMP_GAIN,
                        payload,
                    );
                }
                trace!("hda: NID={}: output amp unmuted, gain=0x7F", nid);
            }

            // Also unmute input amps (for mixers/selectors in the path)
            let has_in_amp = self
                .controller
                .codecs
                .iter()
                .find(|c| c.address == codec_addr)
                .and_then(|c| c.widgets.iter().find(|w| w.nid == nid))
                .is_some_and(|w| w.caps & WIDGET_CAP_IN_AMP != 0);

            if has_in_amp {
                // Set input amp index 0: both channels, unmuted, max gain
                let payload =
                    AMP_SET_INPUT | AMP_SET_LEFT | AMP_SET_RIGHT | 0x7F;
                unsafe {
                    self.controller.corb_rirb.send_verb_long(
                        codec_addr,
                        nid,
                        VERB_SET_AMP_GAIN,
                        payload,
                    );
                }
                trace!("hda: NID={}: input amp unmuted, gain=0x7F", nid);
            }

            // Power on each widget
            unsafe {
                self.controller.corb_rirb.set_verb(
                    codec_addr,
                    nid,
                    VERB_SET_POWER_STATE,
                    0x00,
                );
            }
            trace!("hda: NID={}: powered on (D0)", nid);
        }

        info!("hda: output path unmuted ({} widgets)", all_nids.len());
    }
}
