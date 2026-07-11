# MindBell

A wearable mindfulness bell: an **ESP32-C3** device that gives short vibration
pulses on a user-defined schedule, configured over BLE from a Flutter app.

Set it to nudge you every 7 minutes during the day (09:00–20:00), every
20 minutes in the evening (20:00–23:00), and stay silent at night — any number
of time segments, each with its own interval, editable from your phone.

## How it works

```
┌─────────────────────┐   BLE (on demand)   ┌──────────────────────┐
│  Flutter app         │ ◄─────────────────► │  ESP32-C3 firmware   │
│  scan · edit         │  schedule (JSON)    │  Rust, esp-idf-svc   │
│  schedule · sync     │  time (u64 LE)      │  NimBLE · NVS · RTC  │
│  time                │                     │  deep sleep ↔ buzz   │
└─────────────────────┘                     └──────────────────────┘
```

The core design decision: **the device lives in deep sleep, not in BLE.**
The ESP32-C3's internal RTC keeps counting through deep sleep; the chip wakes
on schedule, pulses the vibration motor, and sleeps again. A button press wakes
it into a short BLE advertising window (30–60 s) for configuration — that's
what makes a 450 mAh LiPo last for weeks instead of a day.

## Firmware (`firmware/`)

Rust on `esp-idf-svc` (ESP-IDF v5.2.2) with `esp32-nimble` for BLE:

- Schedule stored in NVS — survives reboots and battery swaps
- RTC timekeeping with **drift auto-calibration** (timezone-safe); phone syncs
  Unix time on every connection
- Schedule model: array of `{ start, end, interval_min, enabled }` segments;
  overlap and gap handling on-device
- Custom partition table (3 MB factory image for the BLE stack)

Build: standard `esp-idf` Rust toolchain (`espup`), then

```bash
cd firmware
cargo run --release   # builds and flashes via espflash, opens monitor
```

See `firmware/README.md` for toolchain-version pinning notes (the
`esp-idf-svc` / `esp32-nimble` MSRV window is narrow).

## App (`app/`)

Flutter + `flutter_blue_plus`: scans for the **MindBell** device, edits the
schedule with clock-face time pickers, syncs time. Talks to the same BLE
characteristics you could poke manually with nRF Connect:

- service `6d696e64-6265-6c6c-0000-000000000000` (`mindbell` in ASCII)
- schedule `…0001` (JSON blob), time `…0003` (u64 LE)

Setup and Android BLE-permission notes: `app/README.md`.

## Hardware

ESP32-C3 board, coin vibration motor, LiPo 602040 450 mAh, USB-C charging
(TP4056). The whole power chain is designed around **quiescent current in deep
sleep** — the number that actually decides wearable battery life.
Detailed hardware and power-budget notes (in Russian): [`hardware_notes.md`](hardware_notes.md).

## Status

v0.1.1 — working prototype: firmware and app talk to each other, schedule and
time sync work end-to-end. No releases yet; PCB/enclosure not finalized.
