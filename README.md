# RCCar – ESP32-C3 SuperMini BLE Kontrol

ESP32-C3 SuperMini üzerinde çalışan, Nordic UART Service (NUS) kullanan BLE kontrollü RC araba firmware'i.

## Bağlantı

- Cihaz adı: **RCCar**
- Profil: Nordic UART Service (NUS) – Serial Bluetooth Terminal, nRF Connect vb. uygulamalarla ekstra ayar olmadan bağlanır.
- Komutlar tek karakter olarak gönderilir (`w`, `a`, `s`, `d`, `x` veya boşluk).

## Pin Atamaları

| Komut | Anlamı     | GPIO  | Açıklama                  |
|-------|------------|-------|---------------------------|
| `w`   | İleri      | **4** | Forward pin HIGH          |
| `s`   | Geri       | **5** | Backward pin HIGH         |
| `a`   | Sol        | **6** | Left pin HIGH             |
| `d`   | Sağ        | **7** | Right pin HIGH            |
| `x` / boşluk | Dur   | —     | Tüm yön pinleri LOW       |
| —     | Status LED | **8** | Onboard LED (active LOW)  |

- Her komutta ilgili yön pini HIGH yapılır, diğerleri LOW yapılır.
- Her komutta onboard LED (GPIO8) kısa süre yanıp söner.
- LED active-LOW olduğu için `LOW` = yanık, `HIGH` = sönük.

## Derleme & Yükleme

```bash
cargo build --release
# veya espflash / probe-rs ile yükle
```

## Notlar

- GPIO4–7 güvenli kullanım pinleridir.
- GPIO8 strapping pin olsa da LED için standarttır (active LOW).
- Motor sürücü (L298N, DRV8833 vb.) bağlarken yön pinlerini sürücünün IN pinlerine, ortak GND’yi paylaşmaya dikkat edin.
