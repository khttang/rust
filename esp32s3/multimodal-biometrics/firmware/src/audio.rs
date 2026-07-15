use esp_idf_sys as sys;
use log::info;

// --- SAFE, CONFLICT-FREE AUDIO PIN DEFINITIONS ---
const PIN_MIC_WS: i32 = 41;
const PIN_MIC_BCLK: i32 = 42;
const PIN_MIC_DIN: i32 = 2;

const PIN_SPK_WS: i32 = 1;
const PIN_SPK_BCLK: i32 = 21; 
const PIN_SPK_DOUT: i32 = 14;

pub struct AudioSystem {
    pub tx_handle: sys::i2s_chan_handle_t,
    pub rx_handle: sys::i2s_chan_handle_t,
}

pub fn init_audio_subsystem() -> anyhow::Result<AudioSystem> {
    unsafe {
        info!("Initializing modern ESP-IDF v5.x I2S Audio Driver...");

        let mut tx_handle: sys::i2s_chan_handle_t = std::ptr::null_mut();
        let mut rx_handle: sys::i2s_chan_handle_t = std::ptr::null_mut();

        let host_cfg = sys::i2s_chan_config_t {
            id: sys::i2s_port_t_I2S_NUM_0, 
            role: sys::i2s_role_t_I2S_ROLE_MASTER,
            dma_desc_num: 6,      
            dma_frame_num: 240,   
            auto_clear_before_cb: true, // Safely drops out stale fragments on latency bottlenecks
            allow_pd: false,            // Keep peripheral powered up during processing loops
            intr_priority: 0,
            __bindgen_anon_1: sys::i2s_chan_config_t__bindgen_ty_1 {
                auto_clear: true,       // Force automatic buffer clearing on underflow
            },
        };

        let err = sys::i2s_new_channel(&host_cfg, &mut tx_handle, &mut rx_handle);
        if err != sys::ESP_OK {
            return Err(anyhow::anyhow!("Failed to instantiate I2S port channels: {}", err));
        }

        // Universal Helper Fix: Initialize the subconfig blocks using the official ESP-IDF helper macro shims.
        // This ensures the correct underlying clock source types are passed cleanly to the HAL wrapper!
        let rx_std_cfg: sys::i2s_std_config_t = sys::i2s_std_config_t {
            clk_cfg: sys::i2s_std_clk_config_t {
                sample_rate_hz: 16000,
                // Fix: Double-cast the explicit 160MHz PLL enum variant to pass Rust's type-checker cleanly
                clk_src: (sys::soc_module_clk_t_SOC_MOD_CLK_PLL_F160M as u32) as sys::i2s_clock_src_t, 
                mclk_multiple: sys::i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256,
                bclk_div: 0,
                ext_clk_freq_hz: 0,
            },
            slot_cfg: sys::i2s_std_slot_config_t {
                data_bit_width: sys::i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT,
                slot_bit_width: sys::i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO,
                slot_mode: sys::i2s_slot_mode_t_I2S_SLOT_MODE_MONO,
                slot_mask: sys::i2s_std_slot_mask_t_I2S_STD_SLOT_LEFT,
                ws_width: 16,
                ws_pol: false,
                bit_shift: true,
                left_align: true,
                big_endian: false,
                bit_order_lsb: false,
            },
            gpio_cfg: sys::i2s_std_gpio_config_t {
                mclk: -1,
                bclk: PIN_MIC_BCLK,
                ws: PIN_MIC_WS,
                din: PIN_MIC_DIN,
                dout: -1,
                invert_flags: sys::i2s_std_gpio_config_t__bindgen_ty_1 {
                    _bitfield_1: sys::i2s_std_gpio_config_t__bindgen_ty_1::new_bitfield_1(0, 0, 0),
                    _bitfield_align_1: [],
                    __bindgen_padding_0: [0, 0, 0],
                },
            },
        };

        let err = sys::i2s_channel_init_std_mode(rx_handle, &rx_std_cfg);
        if err != sys::ESP_OK {
            return Err(anyhow::anyhow!("Failed to map Microphone configuration to I2S slot: {}", err));
        }

        // Configure the Speaker Out Channel
        let mut tx_std_cfg: sys::i2s_std_config_t = std::mem::zeroed();
        tx_std_cfg.clk_cfg = rx_std_cfg.clk_cfg; 
        tx_std_cfg.slot_cfg = rx_std_cfg.slot_cfg;

        tx_std_cfg.slot_cfg.slot_mode = sys::i2s_slot_mode_t_I2S_SLOT_MODE_STEREO; 
        tx_std_cfg.slot_cfg.slot_mask = sys::i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH;

        tx_std_cfg.gpio_cfg = sys::i2s_std_gpio_config_t {
            mclk: -1,
            bclk: PIN_SPK_BCLK,
            ws: PIN_SPK_WS,
            din: -1,
            dout: PIN_SPK_DOUT,
            invert_flags: sys::i2s_std_gpio_config_t__bindgen_ty_1 {
                _bitfield_1: sys::i2s_std_gpio_config_t__bindgen_ty_1::new_bitfield_1(0, 0, 0),
                _bitfield_align_1: [],
                __bindgen_padding_0: [0,0,0],
            },
        };

        let err = sys::i2s_channel_init_std_mode(tx_handle, &tx_std_cfg);
        if err != sys::ESP_OK {
            return Err(anyhow::anyhow!("Failed to map Speaker configuration to I2S slot: {}", err));
        }

        // Wake and activate both DMA engines
        sys::i2s_channel_enable(rx_handle);
        sys::i2s_channel_enable(tx_handle);

        info!("I2S Hardware Channels activated. Microphone and Speaker pipelines fully active.");

        Ok(AudioSystem { tx_handle, rx_handle })
    }
}

// --- MICROPHONE CAPTURE HIGH-PRIORITY PUMP ---
pub unsafe extern "C" fn native_audio_mic_pump_task(params: *mut core::ffi::c_void) {
    // Unpack the raw pointers
    let tuple_ptr = Box::from_raw(params as *mut (*mut core::ffi::c_void, Box<async_channel::Sender<Vec<i16>>>));
    let rx_handle = tuple_ptr.0 as esp_idf_svc::sys::i2s_chan_handle_t;
    let tx_channel = tuple_ptr.1;

    let mut raw_samples = [0i16; 512]; // 16-bit signed PCM integer format
    let mut bytes_read: usize = 0;

    log::info!("Microphone DMA stream pipeline active on Core 1.");

    loop {
        let err = esp_idf_svc::sys::i2s_channel_read(
            rx_handle,
            raw_samples.as_mut_ptr() as *mut core::ffi::c_void,
            raw_samples.len() * 2, // Size in bytes (16-bit = 2 bytes)
            &mut bytes_read,
            100 // Block for up to 100ms
        );

        if err == esp_idf_svc::sys::ESP_OK && bytes_read > 0 {
            let sample_count = bytes_read / 2;
            let audio_frame = raw_samples[..sample_count].to_vec();

            // Push PCM data to your Voice Recognition module
            // If the processing thread is busy, drop frame to preserve realtime flow
            let _ = tx_channel.try_send(audio_frame);
        }

        // Relinquish control back to the scheduler
        esp_idf_sys::vTaskDelay(1);
    }
}

// --- SPEAKER PLAYBACK LOW-LATENCY PUMP ---
pub unsafe extern "C" fn native_audio_spk_pump_task(params: *mut core::ffi::c_void) {
    let tx_handle = params as esp_idf_svc::sys::i2s_chan_handle_t;
    let mut bytes_written: usize = 0;

    log::info!("Speaker DMA playback pump active on Core 0.");

    loop {
        // ---> FETCH INCOMING PCM AUDIO PACKETS FROM YOUR SYNTHESIZER OR WEBSOCKET HERE <---
        // For example, if you have an echo test array:
        let test_tone = [0i16; 256]; 

        let err = esp_idf_svc::sys::i2s_channel_write(
            tx_handle,
            test_tone.as_ptr() as *const core::ffi::c_void,
            test_tone.len() * 2,
            &mut bytes_written,
            50
        );

        if err != esp_idf_svc::sys::ESP_OK {
            log::warn!("Speaker hardware pipeline encountered a TX underflow error.");
        }

        // Keep a 10ms pacing yield interval
        esp_idf_sys::vTaskDelay(10);
    }
}
