//! MindBell — прошивка носимой вибро-«напоминалки» на ESP32-C3.
//!
//! Архитектура (без кнопки и без deep sleep):
//!   * устройство постоянно бодрствует (опционально — automatic light sleep,
//!     см. фичу `light_sleep`), поэтому BLE всегда в эфире и телефон может в
//!     любой момент подключиться: синхронизировать время и/или прислать
//!     расписание;
//!   * основной цикл по расписанию даёт виброимпульсы на мотор внутри активных
//!     окон;
//!   * расписание хранится в NVS и переживает перезагрузку;
//!   * время суток ведётся системными часами; дата не важна — расписание
//!     оперирует только временем суток, поэтому телефон шлёт «локальный epoch»
//!     (UTC + смещение зоны).
//!
//! Почему нет deep sleep / кнопки: на этой ревизии платы (мотор и LDO висят
//! прямо на выходе TP4056, без load-sharing, GPIO-затвор без надёжного Rpd)
//! deep sleep провоцировал ложные пробуждения/просадки и хаотичную вибрацию.
//! Постоянное бодрствование держит GPIO3 активно в LOW и даёт непрерывный лог
//! по USB-Serial-JTAG.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;

use esp32_nimble::utilities::mutex::Mutex as NimbleMutex;
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{
    uuid128, BLEAdvertisementData, BLECharacteristic, BLEDevice, NimbleProperties,
};

mod schedule;
use schedule::{Schedule, SECONDS_PER_DAY};

// ─────────────────────────────────────────────────────────────────────────────
// КОНФИГУРАЦИЯ — подгоните под свою разводку платы!
// ─────────────────────────────────────────────────────────────────────────────

// Пин затвора MOSFET вибромотора (low-side ключ) — GPIO3. Задаётся ниже как
// peripherals.pins.gpio3. Пока устройство бодрствует, пин активно держится в
// LOW, поэтому ложные импульсы из-за плавающего затвора исключены.

/// Сигнал-напоминание (по расписанию): 2 коротких импульса по 180 мс с паузой
/// 120 мс между ними.
const REMINDER_PULSES: u32 = 2;
const REMINDER_PULSE_MS: u32 = 180;
const REMINDER_GAP_MS: u32 = 120;

/// Короткое виброподтверждение (телефон синхронизировал время / прислал
/// расписание) — 1 длинный импульс, чтобы отличать от напоминания.
const CONFIRM_PULSES: u32 = 1;
const CONFIRM_PULSE_MS: u32 = 300;
const CONFIRM_GAP_MS: u32 = 0;

/// Имя устройства в BLE-рекламе.
const DEVICE_NAME: &str = "MindBell";

/// Ключ расписания в NVS.
const NVS_KEY: &str = "sched";

/// Шаг основного цикла, с. Каждый тик применяем присланное по BLE и пересчитываем
/// расписание — компромисс между отзывчивостью на изменения и нагрузкой.
const TICK_SEC: u32 = 10;

// UUID сервиса и характеристик (произвольные 128-бит). Те же должны быть в
// приложении на Flutter.
const SERVICE_UUID: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000000");
/// Чтение/запись расписания (JSON, как в schedule.rs).
const CHAR_CONFIG: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000001");
/// Синхронизация времени: запись u64 little-endian — «локальный epoch» (сек).
const CHAR_TIME: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000003");
// Примечание: характеристика батареи (…0002) зарезервирована на будущее.

// ─────────────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Причина последнего сброса — ключевая улика. Повторяющийся BROWNOUT/PANIC/WDT
    // в логе означает, что устройство перезапускается (а не «логика расписания»).
    // Теперь, без deep sleep, лог не прерывается — эту строку видно на каждом
    // перезапуске.
    log_reset_reason();

    // Опциональный automatic light sleep (BLE остаётся в эфире, CPU дремлет между
    // событиями). По умолчанию ВЫКЛ — чтобы при отладке лог шёл непрерывно.
    #[cfg(feature = "light_sleep")]
    enable_light_sleep();

    let peripherals = Peripherals::take()?;
    let nvs_part = EspDefaultNvsPartition::take()?;
    let mut nvs = EspNvs::new(nvs_part, "mindbell", true)?;

    // Мотор: выключен по умолчанию и активно держится в LOW всё время работы.
    let mut motor = PinDriver::output(peripherals.pins.gpio3)?;
    motor.set_low()?;

    // Поднимаем BLE один раз и навсегда: реклама + GATT-сервер живут в задаче
    // NimBLE, основной цикл им не мешает.
    let ble = start_ble(&load_schedule(&mut nvs).to_json())?;

    log::info!("MindBell up: BLE advertising as '{}', schedule loop running", DEVICE_NAME);

    // Основной цикл. Никакого сна с потерей состояния — просто крутимся и
    // жужжим по расписанию, попутно применяя то, что прислал телефон.
    loop {
        // 1) Применяем присланное по BLE.
        if let Some(t) = ble.pending_time.lock().unwrap().take() {
            set_local_time(t);
            buzz(&mut motor, CONFIRM_PULSES, CONFIRM_PULSE_MS, CONFIRM_GAP_MS);
        }
        if let Some(bytes) = ble.pending_sched.lock().unwrap().take() {
            match Schedule::from_json(&bytes) {
                Ok(s) => {
                    let json = s.to_json();
                    if let Err(e) = nvs.set_blob(NVS_KEY, &json) {
                        log::warn!("nvs save failed: {e}");
                    } else {
                        log::info!("schedule updated: {} segments", s.segments.len());
                    }
                    // Чтобы читающий телефон видел актуальное значение.
                    ble.cfg_char.lock().set_value(&json);
                    buzz(&mut motor, CONFIRM_PULSES, CONFIRM_PULSE_MS, CONFIRM_GAP_MS);
                }
                Err(e) => log::warn!("rejected invalid schedule: {e}"),
            }
        }

        // 2) Считаем, когда ближайший слот, и жужжим, когда до него дошли.
        match now_local_sec() {
            None => {
                // Время ещё не выставлено — ждём синхронизации по BLE.
                log::info!("time not set — waiting for phone sync (BLE)");
                FreeRtos::delay_ms(TICK_SEC * 1000);
            }
            Some(now) => {
                let sched = load_schedule(&mut nvs);
                match sched.seconds_until_next(now) {
                    Some(s) if s <= TICK_SEC => {
                        // Слот в пределах текущего тика — досыпаем до него точно.
                        FreeRtos::delay_ms(s * 1000);
                        // Перепроверяем окно (вдруг расписание/время сменились).
                        match now_local_sec() {
                            Some(n2) if sched.in_active_segment(n2) => {
                                log::info!("buzz @ {}s-of-day", n2);
                                buzz(
                                    &mut motor,
                                    REMINDER_PULSES,
                                    REMINDER_PULSE_MS,
                                    REMINDER_GAP_MS,
                                );
                            }
                            _ => {}
                        }
                        // Шагаем за момент слота, чтобы не пересчитать тот же слот.
                        FreeRtos::delay_ms(800);
                    }
                    Some(s) => {
                        // До слота далеко — спим грубым тиком и снова проверяем
                        // (заодно подхватим изменения, присланные телефоном).
                        FreeRtos::delay_ms(s.min(TICK_SEC) * 1000);
                    }
                    None => {
                        // Нет включённых сегментов — просто ждём настройки.
                        FreeRtos::delay_ms(TICK_SEC * 1000);
                    }
                }
            }
        }
    }
}

/// Печатает причину последнего сброса. `ESP_RST_BROWNOUT` в цикле = просадка
/// питания; `ESP_RST_PANIC` = паника прошивки; `ESP_RST_WDT/TASK/INT` = завис.
fn log_reset_reason() {
    let r = unsafe { sys::esp_reset_reason() };
    let name = match r {
        sys::esp_reset_reason_t_ESP_RST_POWERON => "POWERON",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "BROWNOUT",
        sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "DEEPSLEEP",
        sys::esp_reset_reason_t_ESP_RST_PANIC => "PANIC",
        sys::esp_reset_reason_t_ESP_RST_WDT => "WDT",
        sys::esp_reset_reason_t_ESP_RST_SW => "SW",
        sys::esp_reset_reason_t_ESP_RST_EXT => "EXT",
        sys::esp_reset_reason_t_ESP_RST_USB => "USB",
        sys::esp_reset_reason_t_ESP_RST_JTAG => "JTAG",
        _ => "OTHER",
    };
    log::info!("reset reason: {} ({})", name, r);
    if r == sys::esp_reset_reason_t_ESP_RST_BROWNOUT {
        log::warn!("BROWNOUT! питание просело — мотор/инраш/нет C_bulk/не-logic-level MOSFET");
    }
}

/// Серия виброимпульсов: `pulses` импульсов по `on_ms` мс, паузы `gap_ms` мс
/// ставятся ТОЛЬКО между импульсами (после последнего паузы нет).
fn buzz<P, MODE>(motor: &mut PinDriver<'_, P, MODE>, pulses: u32, on_ms: u32, gap_ms: u32)
where
    P: esp_idf_svc::hal::gpio::Pin,
    MODE: esp_idf_svc::hal::gpio::OutputMode,
{
    #[cfg(feature = "diag")]
    {
        let _ = (motor, on_ms, gap_ms);
        log::info!("[diag] buzz suppressed ({} pulses) — motor not driven", pulses);
    }
    #[cfg(not(feature = "diag"))]
    for i in 0..pulses {
        let _ = motor.set_high();
        FreeRtos::delay_ms(on_ms);
        let _ = motor.set_low();
        if i + 1 < pulses {
            FreeRtos::delay_ms(gap_ms);
        }
    }
}

/// Загружает расписание из NVS; при отсутствии/ошибке — демо-расписание.
fn load_schedule(nvs: &mut EspNvs<NvsDefault>) -> Schedule {
    let mut buf = [0u8; 2048];
    match nvs.get_blob(NVS_KEY, &mut buf) {
        Ok(Some(bytes)) => Schedule::from_json(bytes).unwrap_or_else(|e| {
            log::warn!("bad schedule in NVS ({e}), using demo");
            Schedule::default_demo()
        }),
        _ => {
            log::info!("no schedule in NVS, using demo");
            Schedule::default_demo()
        }
    }
}

// ── Время ───────────────────────────────────────────────────────────────────

/// Текущее время суток в секундах (0..86400), либо `None`, если системные часы
/// ещё не синхронизированы.
fn now_local_sec() -> Option<u32> {
    let mut tv = sys::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
    // 1_600_000_000 ≈ 2020-09. Меньше — значит время не выставляли.
    if (tv.tv_sec as i64) < 1_600_000_000 {
        None
    } else {
        Some((tv.tv_sec as u64 % SECONDS_PER_DAY as u64) as u32)
    }
}

/// Выставляет системные часы. `local_epoch` — секунды «локального epoch»
/// (UTC + смещение часового пояса), присланные телефоном.
fn set_local_time(local_epoch: u64) {
    let tv = sys::timeval { tv_sec: local_epoch as sys::time_t, tv_usec: 0 };
    unsafe { sys::settimeofday(&tv, core::ptr::null()) };
    log::info!("time synced: epoch={}", local_epoch);
}

// ── BLE (постоянная реклама + GATT) ───────────────────────────────────────────

/// Хэндлы BLE, нужные основному циклу: характеристика расписания (чтобы обновлять
/// отдаваемое значение) и буферы, в которые колбэки складывают присланное.
struct Ble {
    // create_characteristic возвращает Arc с собственным мьютексом NimBLE
    // (не std::sync::Mutex), поэтому тип здесь — NimbleMutex.
    cfg_char: Arc<NimbleMutex<BLECharacteristic>>,
    pending_sched: Arc<Mutex<Option<Vec<u8>>>>,
    pending_time: Arc<Mutex<Option<u64>>>,
}

/// Поднимает BLE-устройство, сервис и характеристики, запускает рекламу и
/// автоматически возобновляет её после отключения телефона. Возвращается сразу —
/// дальше всё крутится в задаче NimBLE.
fn start_ble(initial_sched_json: &[u8]) -> anyhow::Result<Ble> {
    let pending_sched: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let pending_time: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    let connected = Arc::new(AtomicBool::new(false));

    let ble = BLEDevice::take();
    let server = ble.get_server();
    let adv = ble.get_advertising();

    {
        let c = connected.clone();
        server.on_connect(move |_, _| {
            c.store(true, Ordering::SeqCst);
            log::info!("BLE connected");
        });
    }
    {
        let c = connected.clone();
        // После отключения NimBLE останавливает рекламу — поднимаем её снова,
        // иначе телефон больше не сможет подключиться.
        server.on_disconnect(move |_, _| {
            c.store(false, Ordering::SeqCst);
            log::info!("BLE disconnected — restart advertising");
            let _ = adv.lock().start();
        });
    }

    let service = server.create_service(SERVICE_UUID);

    // Характеристика расписания: при чтении отдаём текущее, при записи — копим.
    let cfg_char = service
        .lock()
        .create_characteristic(CHAR_CONFIG, NimbleProperties::READ | NimbleProperties::WRITE);
    cfg_char.lock().set_value(initial_sched_json);
    {
        let p = pending_sched.clone();
        cfg_char.lock().on_write(move |args| {
            *p.lock().unwrap() = Some(args.recv_data().to_vec());
        });
    }

    // Характеристика синхронизации времени (только запись u64 LE).
    let time_char = service.lock().create_characteristic(CHAR_TIME, NimbleProperties::WRITE);
    {
        let p = pending_time.clone();
        time_char.lock().on_write(move |args| {
            let d = args.recv_data();
            if d.len() >= 8 {
                let mut b = [0u8; 8];
                b.copy_from_slice(&d[..8]);
                *p.lock().unwrap() = Some(u64::from_le_bytes(b));
            }
        });
    }

    adv.lock()
        .set_data(BLEAdvertisementData::new().name(DEVICE_NAME).add_service_uuid(SERVICE_UUID))?;
    adv.lock().start()?;

    Ok(Ble { cfg_char, pending_sched, pending_time })
}

// ── Питание ───────────────────────────────────────────────────────────────────

/// Включает automatic light sleep: CPU дремлет между событиями, BLE-контроллер
/// продолжает поддерживать рекламу/соединение (modem sleep). Требует в
/// sdkconfig: CONFIG_PM_ENABLE=y и CONFIG_FREERTOS_USE_TICKLESS_IDLE=y.
#[cfg(feature = "light_sleep")]
fn enable_light_sleep() {
    let cfg = sys::esp_pm_config_t {
        max_freq_mhz: 160,
        min_freq_mhz: 40,
        light_sleep_enable: true,
    };
    let err = unsafe { sys::esp_pm_configure(&cfg as *const _ as *const core::ffi::c_void) };
    if err == sys::ESP_OK {
        log::info!("automatic light sleep enabled (160/40 MHz)");
    } else {
        log::warn!("esp_pm_configure failed: {} — staying fully awake", err);
    }
}
