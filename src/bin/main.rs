#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_time::Timer;
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    interrupt::software::SoftwareInterruptControl,
    timer::timg::TimerGroup,
};
use esp_radio::ble::controller::BleConnector;
use log::{info, warn};
use trouble_host::prelude::*;

esp_bootloader_esp_idf::esp_app_desc!();

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;
/// Max number of L2CAP channels.
const L2CAP_CHANNELS_MAX: usize = 2; // Signal + att

// GATT Server definition - Nordic UART Service (standart, tum Terminal uygulamalari tanir)
#[gatt_server]
struct Server {
    nus: NordicUartService,
}

/// Nordic UART Service
/// Service UUID: 6E400001-B5A3-F393-E0A9-E50E24DCCA9E
#[gatt_service(uuid = "6e400001-b5a3-f393-e0a9-e50e24dcca9e")]
struct NordicUartService {
    /// RX Characteristic (telefon -> cihaz). Komutlar buraya yazilir.
    /// UUID: 6E400002-B5A3-F393-E0A9-E50E24DCCA9E
    #[characteristic(
        uuid = "6e400002-b5a3-f393-e0a9-e50e24dcca9e",
        write,
        write_without_response
    )]
    rx: [u8; 20],

    /// TX Characteristic (cihaz -> telefon). Ileride cevap gondermek icin.
    /// UUID: 6E400003-B5A3-F393-E0A9-E50E24DCCA9E
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

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    info!("RCCar BLE baslatiliyor (ESP32-C3 SuperMini) - Nordic UART Service");

    let connector = BleConnector::new(peripherals.BT, Default::default()).unwrap();
    let controller: ExternalController<_, 1> = ExternalController::new(connector);

    ble_run(controller).await;
}

/// Run the BLE stack and accept connections forever.
async fn ble_run<C>(controller: C)
where
    C: Controller,
{
    // Fixed random address for consistent advertising
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

    // Run the BLE host task and the advertise/accept loop concurrently
    let _ = select(ble_task(runner), async {
        loop {
            match advertise("RCCar", &mut peripheral, &server).await {
                Ok(conn) => {
                    info!("Baglandi! Komut bekleniyor (NUS)...");
                    // Handle GATT events until disconnect
                    let _ = gatt_events_task(&server, &conn).await;
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

/// Background task that must keep running the BLE host.
async fn ble_task<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    loop {
        if let Err(e) = runner.run().await {
            panic!("[ble_task] error: {:?}", e);
        }
    }
}

/// Handle GATT events (especially writes to the NUS RX characteristic).
async fn gatt_events_task<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
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
                            // Bosluk, CR, LF ve null karakterleri atla, ilk gecerli karakteri al
                            if let Some(&byte) = data
                                .iter()
                                .find(|&&b| b != b'\r' && b != b'\n' && b != b' ' && b != 0)
                            {
                                let ch = (byte as char).to_ascii_lowercase();
                                match ch {
                                    'w' => info!("'w' basildi -> ILERI"),
                                    's' => info!("'s' basildi -> GERI"),
                                    'a' => info!("'a' basildi -> SOL"),
                                    'd' => info!("'d' basildi -> SAG"),
                                    'x' => info!("'x' basildi -> DUR"),
                                    c if c.is_ascii_alphanumeric() => {
                                        info!("'{}' basildi", c);
                                    }
                                    _ => {
                                        info!("Bilinmeyen komut byte: 0x{:02x}", byte);
                                    }
                                }
                            } else if !data.is_empty() {
                                // Sadece bosluk/CR/LF geldiyse DUR kabul et
                                info!("Bosluk/CR alindi -> DUR");
                            } else {
                                info!("Bos yazma alindi");
                            }
                        }
                    }
                    GattEvent::Read(event) => {
                        if event.handle() == rx_char.handle {
                            info!("[gatt] RX characteristic okundu");
                        }
                    }
                    _ => {}
                }
                // Yaniti gonder
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

/// Advertise and wait for a connection.
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut advertiser_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            // Nordic UART Service UUID (little-endian)
            // 6E400001-B5A3-F393-E0A9-E50E24DCCA9E
            AdStructure::ServiceUuids128(&[[
                0x9e, 0xca, 0xdc, 0x24, 0x0e, 0xe5, 0xa9, 0xe0,
                0x93, 0xf3, 0xa3, 0xb5, 0x01, 0x00, 0x40, 0x6e,
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
