#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    interrupt::software::SoftwareInterruptControl,
    timer::timg::TimerGroup,
};
use esp_radio::ble::controller::BleConnector;
use log::{info, warn};
use static_cell::StaticCell;
use trouble_host::prelude::*;

esp_bootloader_esp_idf::esp_app_desc!();

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;
/// Max number of L2CAP channels.
const L2CAP_CHANNELS_MAX: usize = 2; // Signal + att

// ---- Pin tanimlari (README ile ayni) ----
// W (Ileri)  -> GPIO4
// S (Geri)   -> GPIO5
// A (Sol)    -> GPIO6
// D (Sag)    -> GPIO7
// X / Space  -> hepsini kapat (DUR)
// Status LED -> GPIO8 (onboard, active LOW)

struct Pins {
    forward: Output<'static>,  // W - GPIO4
    backward: Output<'static>, // S - GPIO5
    left: Output<'static>,     // A - GPIO6
    right: Output<'static>,    // D - GPIO7
    led: Output<'static>,      // GPIO8 onboard (active LOW)
}

type PinsMutex = Mutex<CriticalSectionRawMutex, Pins>;
static PINS: StaticCell<PinsMutex> = StaticCell::new();

// GATT Server definition - Nordic UART Service
#[gatt_server]
struct Server {
    nus: NordicUartService,
}

/// Nordic UART Service
/// Service UUID: 6E400001-B5A3-F393-E0A9-E50E24DCCA9E
#[gatt_service(uuid = "6e400001-b5a3-f393-e0a9-e50e24dcca9e")]
struct NordicUartService {
    /// RX Characteristic (telefon -> cihaz)
    #[characteristic(
        uuid = "6e400002-b5a3-f393-e0a9-e50e24dcca9e",
        write,
        write_without_response
    )]
    rx: [u8; 20],

    /// TX Characteristic (cihaz -> telefon)
    #[characteristic(
        uuid = "6e400003-b5a3-f393-e0a9-e50e24dcca9e",
        notify,
        read
    )]
    tx: [u8; 20],
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Heap for BLE stack
    esp_alloc::heap_allocator!(size: 72 * 1024);

    // GPIO pinlerini cikıs olarak ayarla
    let pins = Pins {
        forward: Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default()),
        backward: Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default()),
        left: Output::new(peripherals.GPIO6, Level::Low, OutputConfig::default()),
        right: Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default()),
        // Onboard LED active LOW → baslangicta kapali (HIGH)
        led: Output::new(peripherals.GPIO8, Level::High, OutputConfig::default()),
    };
    let pins = PINS.init(Mutex::new(pins));

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    info!("RCCar BLE baslatiliyor (ESP32-C3 SuperMini) - NUS + GPIO");
    info!("Pinler: W=GPIO4  S=GPIO5  A=GPIO6  D=GPIO7  LED=GPIO8");

    let connector = BleConnector::new(peripherals.BT, Default::default()).unwrap();
    let controller: ExternalController<_, 1> = ExternalController::new(connector);

    ble_run(controller, pins).await;
}

/// Run the BLE stack and accept connections forever.
async fn ble_run<C>(controller: C, pins: &'static PinsMutex)
where
    C: Controller,
{
    let address: Address = Address::random([0x11, 0x22, 0x33, 0x44, 0x55, 0xc3]);
    info!("BLE adresimiz = {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "RCCar",
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    }))
    .unwrap();

    let _ = select(ble_task(runner), async {
        loop {
            match advertise("RCCar", &mut peripheral, &server).await {
                Ok(conn) => {
                    info!("Baglandi! Komut bekleniyor (NUS)...");
                    let _ = gatt_events_task(&server, &conn, pins).await;
                    info!("Baglanti koptu, tekrar yayin basliyor...");
                }
                Err(e) => {
                    warn!("[adv] hata: {:?}", e);
                    Timer::after_millis(500).await;
                }
            }
        }
    })
    .await;
}

async fn ble_task<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    loop {
        if let Err(e) = runner.run().await {
            panic!("[ble_task] error: {:?}", e);
        }
    }
}

/// Komut geldiginde ilgili pinleri ayarla ve LED'i kisa sure yanip sondur.
async fn apply_command(pins: &'static PinsMutex, cmd: char) {
    let mut p = pins.lock().await;

    // Once tum yon pinlerini kapat
    p.forward.set_low();
    p.backward.set_low();
    p.left.set_low();
    p.right.set_low();

    match cmd {
        'w' => {
            p.forward.set_high();
            info!("GPIO4 HIGH (ILERI)");
        }
        's' => {
            p.backward.set_high();
            info!("GPIO5 HIGH (GERI)");
        }
        'a' => {
            p.left.set_high();
            info!("GPIO6 HIGH (SOL)");
        }
        'd' => {
            p.right.set_high();
            info!("GPIO7 HIGH (SAG)");
        }
        'x' | ' ' => {
            info!("Tum yon pinleri LOW (DUR)");
        }
        _ => {}
    }

    // Onboard LED (active LOW) kisa sure yanip sonsun
    for _ in 0..2 {
        p.led.set_low(); // LED ON
        Timer::after(Duration::from_millis(60)).await;
        p.led.set_high(); // LED OFF
        Timer::after(Duration::from_millis(60)).await;
    }
}

async fn gatt_events_task<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    pins: &'static PinsMutex,
) -> Result<(), Error> {
    let rx_char = server.nus.rx;

    let reason = loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => break reason,
            GattConnectionEvent::Gatt { event } => {
                match &event {
                    GattEvent::Write(event) => {
                        if event.handle() == rx_char.handle {
                            let data = event.data();
                            if let Some(&byte) = data
                                .iter()
                                .find(|&&b| b != b'\r' && b != b'\n' && b != 0)
                            {
                                let ch = (byte as char).to_ascii_lowercase();
                                match ch {
                                    'w' => {
                                        info!("'w' -> ILERI");
                                        apply_command(pins, 'w').await;
                                    }
                                    's' => {
                                        info!("'s' -> GERI");
                                        apply_command(pins, 's').await;
                                    }
                                    'a' => {
                                        info!("'a' -> SOL");
                                        apply_command(pins, 'a').await;
                                    }
                                    'd' => {
                                        info!("'d' -> SAG");
                                        apply_command(pins, 'd').await;
                                    }
                                    'x' | ' ' => {
                                        info!("'{}' -> DUR", ch);
                                        apply_command(pins, 'x').await;
                                    }
                                    c if c.is_ascii_alphanumeric() => {
                                        info!("'{}' basildi (tanimli degil)", c);
                                    }
                                    _ => {
                                        info!("Bilinmeyen komut byte: 0x{:02x}", byte);
                                    }
                                }
                            } else {
                                info!("Bos yazma alindi");
                            }
                        }
                    }
                    GattEvent::Read(_) => {}
                    _ => {}
                }
                match event.accept() {
                    Ok(reply) => {
                        reply.send().await;
                    }
                    Err(e) => warn!("[gatt] yanit gonderilemedi: {:?}", e),
                }
            }
            _ => {}
        }
    };
    info!("[gatt] baglanti kapandi: {:?}", reason);
    Ok(())
}

async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut advertiser_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids128(&[[
                0x9e, 0xca, 0xdc, 0x24, 0x0e, 0xe5, 0xa9, 0xe0, 0x93, 0xf3, 0xa3, 0xb5, 0x01, 0x00,
                0x40, 0x6e,
            ]]),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut advertiser_data[..],
    )?;

    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..len],
                scan_data: &[],
            },
        )
        .await?;

    info!("[adv] yayin yapiyor: {} (NUS)", name);
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    info!("[adv] baglanti kuruldu");
    Ok(conn)
}
