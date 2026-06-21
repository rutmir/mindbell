//! MindBell — прошивка носимой вибро-«напоминалки» на ESP32-C3.
//!
//! Логика работы (см. ../../hardware_notes.md):
//!   * устройство почти всё время в deep sleep;
//!   * просыпается по таймеру RTC ровно к моменту очередного вибросигнала —
//!     даёт импульс на мотор и снова засыпает до следующего слота;
//!   * просыпается по кнопке — поднимает BLE на короткое окно, чтобы телефон
//!     синхронизировал время и/или прислал новое расписание;
//!   * расписание хранится в NVS и переживает сон/перезагрузку;
//!   * время суток ведётся системными часами, которые ESP-IDF сохраняет
//!     через deep sleep (дата не важна — расписание оперирует только временем
//!     суток, поэтому телефон шлёт «локальный epoch» = UTC + смещение зоны).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;

use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{uuid128, BLEAdvertisementData, BLEDevice, NimbleProperties};

mod schedule;
use schedule::{Schedule, SECONDS_PER_DAY};

// ─────────────────────────────────────────────────────────────────────────────
// КОНФИГУРАЦИЯ — подгоните под свою разводку платы!
// ─────────────────────────────────────────────────────────────────────────────

// Пин затвора MOSFET вибромотора (low-side ключ) — GPIO3. Задаётся ниже как
// peripherals.pins.gpio3. Должен иметь внешний pulldown (Rpd), чтобы в deep
// sleep мотор был выключен.

/// Пин отдельной кнопки настройки — GPIO4. ДОЛЖЕН быть из GPIO0..=GPIO5 (только
/// они будят C3 из deep sleep). Кнопка замыкает на GND, нужен внешний pull-up
/// (10–100 кОм) на 3V3. Тот же номер используется ниже как peripherals.pins.gpio4.
const BUTTON_GPIO_NUM: u32 = 4;

/// Сколько держать кнопку, чтобы войти в настройку (защита от случайных
/// нажатий в кармане), мс.
const LONG_PRESS_MS: u32 = 2_000;

/// Сигнал-напоминание (по таймеру): 2 коротких импульса по 180 мс с паузами 120 мс
/// между ними.
const REMINDER_PULSES: u32 = 2;
const REMINDER_PULSE_MS: u32 = 180;
const REMINDER_GAP_MS: u32 = 120;


/// Короткое виброподтверждение (вход в режим настройки) — намеренно отличается
/// от напоминания, чтобы их не путать: 1 длинный импульс.
const CONFIRM_PULSES: u32 = 1;
const CONFIRM_PULSE_MS: u32 = 300;
const CONFIRM_GAP_MS: u32 = 700;

/// Имя устройства в BLE-рекламе.
const DEVICE_NAME: &str = "MindBell";

/// Окно рекламы, если телефон не подключился, мс.
const IDLE_WINDOW_MS: u32 = 30_000;
/// Максимальная длительность сессии настройки, мс (страховка).
const MAX_WINDOW_MS: u32 = 300_000;

/// Ключ расписания в NVS.
const NVS_KEY: &str = "sched";

// UUID сервиса и характеристик (произвольные 128-бит). Те же должны быть в
// приложении на Flutter.
const SERVICE_UUID: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000000");
/// Чтение/запись расписания (JSON, как в schedule.rs).
const CHAR_CONFIG: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000001");
/// Синхронизация времени: запись u64 little-endian — «локальный epoch» (сек).
const CHAR_TIME: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000003");
// Примечание: характеристика батареи (…0002) зарезервирована на будущее —
// измерение заряда пока не разведено (см. hardware_notes.md §5).

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Wake {
    Timer,
    Button,
    Cold,
}

fn main() -> anyhow::Result<()> {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let nvs_part = EspDefaultNvsPartition::take()?;
    let mut nvs = EspNvs::new(nvs_part, "mindbell", true)?;

    // Снимаем «защёлку» пинов, выставленную перед прошлым deep sleep (см.
    // enter_deep_sleep), иначе ими нельзя управлять после пробуждения.
    unsafe {
        sys::gpio_deep_sleep_hold_dis();
        sys::gpio_hold_dis(BUTTON_GPIO_NUM as sys::gpio_num_t);
    }

    // Мотор: выключен по умолчанию (Rpd подстрахует, но явно тоже).
    let mut motor = PinDriver::output(peripherals.pins.gpio3)?;
    motor.set_low()?;

    let cause = wakeup_cause();

    // Диагностика тактирования/времени. Сравнивая `raw_epoch` соседних
    // пробуждений с предыдущим «next buzz in N s» из лога, видно реальный дрейф
    // RTC относительно расчётного интервала (см. README, раздел про логи).
    {
        let mut tv = sys::timeval { tv_sec: 0, tv_usec: 0 };
        unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
        log::info!(
            "wakeup: cause={:?} raw_epoch={} time_of_day={:?}",
            cause,
            tv.tv_sec as i64,
            now_local_sec(),
        );
    }

    match cause {
        Wake::Timer => {
            // Жужжим ТОЛЬКО если реально внутри активного окна расписания. Иначе
            // это «страховочное» пробуждение (слотов нет / время потеряно) —
            // молча пересчитываем и снова засыпаем. Проверка по окну (часы), а не
            // по точному слоту, устойчива к дрейфу RTC: даже проснувшись на
            // минуты позже, мы всё ещё «внутри» диапазона.
            match now_local_sec() {
                Some(now) if load_schedule(&mut nvs).in_active_segment(now) => {
                    log::info!("wake: timer -> buzz");
                    buzz(&mut motor, REMINDER_PULSES, REMINDER_PULSE_MS, REMINDER_GAP_MS);
                }
                Some(_) => log::info!("wake: timer outside active window — no buzz"),
                None => log::warn!("wake: timer but time not set — no buzz (need sync)"),
            }
        }
        Wake::Button => {
            // Проснулись по кнопке, но входим в настройку только если её реально
            // удержали LONG_PRESS_MS (анти-случайное нажатие в кармане).
            let mut btn = PinDriver::input(peripherals.pins.gpio4)?;
            btn.set_pull(Pull::Up)?;
            let mut held = 0u32;
            while btn.is_low() && held < LONG_PRESS_MS {
                FreeRtos::delay_ms(50);
                held += 50;
            }
            if held >= LONG_PRESS_MS {
                log::info!("long press -> config mode");
                buzz(&mut motor, CONFIRM_PULSES, CONFIRM_PULSE_MS, CONFIRM_GAP_MS); // «поймал»
                run_config_mode(&mut nvs)?;
            } else {
                log::info!("short press ({} ms) ignored", held);
            }
        }
        Wake::Cold => {
            // Первое включение или потеря питания: часы не идут — поднимаем BLE,
            // чтобы телефон синхронизировал время (иначе расписание не работает).
            log::info!("cold boot");
            if now_local_sec().is_none() {
                log::info!("time not set -> config mode for sync");
                buzz(&mut motor, CONFIRM_PULSES, CONFIRM_PULSE_MS, CONFIRM_GAP_MS);
                run_config_mode(&mut nvs)?;
            }
        }
    }

    // Перечитываем расписание (режим настройки мог его изменить) и считаем,
    // когда нас будить.
    let schedule = load_schedule(&mut nvs);
    let next = now_local_sec().and_then(|now| schedule.seconds_until_next(now));
    match next {
        Some(s) => log::info!("next buzz in {} s", s),
        None => log::warn!("no time/segments — sleeping until button"),
    }

    enter_deep_sleep(next);
}

/// Серия виброимпульсов: `pulses` импульсов по `on_ms` мс, паузы `gap_ms` мс
/// ставятся ТОЛЬКО между импульсами (после последнего паузы нет).
fn buzz<P, MODE>(motor: &mut PinDriver<'_, P, MODE>, pulses: u32, on_ms: u32, gap_ms: u32)
where
    P: esp_idf_svc::hal::gpio::Pin,
    MODE: esp_idf_svc::hal::gpio::OutputMode,
{
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
/// ещё не синхронизированы (после холодного старта).
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
/// (UTC + смещение часового пояса), присланные телефоном. Дата нас не волнует,
/// важно только время суток.
fn set_local_time(local_epoch: u64) {
    let tv = sys::timeval { tv_sec: local_epoch as sys::time_t, tv_usec: 0 };
    unsafe { sys::settimeofday(&tv, core::ptr::null()) };
    log::info!("time synced: epoch={}", local_epoch);
}

// ── Режим настройки (BLE) ─────────────────────────────────────────────────────

fn run_config_mode(nvs: &mut EspNvs<NvsDefault>) -> anyhow::Result<()> {
    // Буферы, в которые BLE-колбэки складывают присланное; применяем после окна.
    let pending_sched: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let pending_time: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    let connected = Arc::new(AtomicBool::new(false));

    let ble = BLEDevice::take();
    let server = ble.get_server();
    {
        let c = connected.clone();
        server.on_connect(move |_, _| c.store(true, Ordering::SeqCst));
    }
    {
        let c = connected.clone();
        server.on_disconnect(move |_, _| c.store(false, Ordering::SeqCst));
    }

    let service = server.create_service(SERVICE_UUID);

    // Характеристика расписания: при чтении отдаём текущее, при записи — копим.
    let cfg_char = service
        .lock()
        .create_characteristic(CHAR_CONFIG, NimbleProperties::READ | NimbleProperties::WRITE);
    cfg_char.lock().set_value(&load_schedule(nvs).to_json());
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

    let adv = ble.get_advertising();
    adv.lock()
        .set_data(BLEAdvertisementData::new().name(DEVICE_NAME).add_service_uuid(SERVICE_UUID))?;
    adv.lock().start()?;
    log::info!("advertising as '{}'", DEVICE_NAME);

    // Держим окно: если никто не подключился — IDLE_WINDOW_MS, иначе до отключения
    // (но не дольше MAX_WINDOW_MS).
    let mut waited: u32 = 0;
    loop {
        FreeRtos::delay_ms(500);
        waited += 500;
        let conn = connected.load(Ordering::SeqCst);
        if !conn && waited >= IDLE_WINDOW_MS {
            break;
        }
        if waited >= MAX_WINDOW_MS {
            break;
        }
    }
    let _ = adv.lock().stop();

    // Применяем присланное.
    if let Some(t) = pending_time.lock().unwrap().take() {
        set_local_time(t);
    }
    if let Some(bytes) = pending_sched.lock().unwrap().take() {
        match Schedule::from_json(&bytes) {
            Ok(s) => {
                nvs.set_blob(NVS_KEY, &s.to_json())?;
                log::info!("schedule updated: {} segments", s.segments.len());
            }
            Err(e) => log::warn!("rejected invalid schedule: {e}"),
        }
    }

    Ok(())
}

// ── Сон / пробуждение ──────────────────────────────────────────────────────────

fn wakeup_cause() -> Wake {
    match unsafe { sys::esp_sleep_get_wakeup_cause() } {
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_TIMER => Wake::Timer,
        sys::esp_sleep_source_t_ESP_SLEEP_WAKEUP_GPIO => Wake::Button,
        // Всё остальное (первое включение, потеря питания, brownout) — холодный старт.
        _ => Wake::Cold,
    }
}

/// Засыпает в deep sleep. Будит таймер (следующий слот) и кнопка (низкий уровень
/// на BUTTON_GPIO). Если слота нет — спит до нажатия кнопки.
fn enter_deep_sleep(next_secs: Option<u32>) -> ! {
    unsafe {
        // Кнопка GPIO4 будит по низкому уровню (кнопка замыкает на GND). На плате
        // есть внешний pull-up 80 кОм (hardware_notes, распиновка), поэтому в deep
        // sleep вход НЕ плавает и держать пад через gpio_hold не нужно. Более того,
        // защёлкивание пада (gpio_hold_en) подозревается в «заморозке» входа, из-за
        // чего переход в LOW не детектировался и кнопка переставала будить, — hold
        // НЕ включаем, полагаемся на внешний pull-up.
        sys::gpio_set_direction(
            BUTTON_GPIO_NUM as sys::gpio_num_t,
            sys::gpio_mode_t_GPIO_MODE_INPUT,
        );
        sys::gpio_set_pull_mode(
            BUTTON_GPIO_NUM as sys::gpio_num_t,
            sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY,
        );

        // Если кнопку всё ещё держат — ждём отпускания (до 3 с). Иначе уровень LOW
        // разбудит нас мгновенно сразу после засыпания → бесконечный цикл
        // wake/sleep (и лишний разряд батареи).
        let mut held_guard = 0u32;
        while sys::gpio_get_level(BUTTON_GPIO_NUM as sys::gpio_num_t) == 0 && held_guard < 3_000 {
            FreeRtos::delay_ms(50);
            held_guard += 50;
        }

        // Пробуждение по кнопке: низкий уровень на BUTTON_GPIO (кнопка на GND).
        sys::esp_deep_sleep_enable_gpio_wakeup(
            1u64 << BUTTON_GPIO_NUM,
            sys::esp_deepsleep_gpio_wake_up_mode_t_ESP_GPIO_WAKEUP_GPIO_LOW,
        );

        // ВСЕГДА ставим таймер: реальный слот, либо страховочные сутки. Это
        // защищает от «вечного сна»: даже если слотов нет / время потеряно /
        // кнопочное пробуждение почему-то не сработало, устройство гарантированно
        // проснётся, пересчитает расписание и даст шанс синхронизироваться.
        const FALLBACK_SECS: u32 = 24 * 3600;
        let sleep_s = next_secs.map(|s| s.max(1)).unwrap_or(FALLBACK_SECS).min(FALLBACK_SECS);
        sys::esp_sleep_enable_timer_wakeup(sleep_s as u64 * 1_000_000);

        log::info!("entering deep sleep for {} s (gpio+timer armed)", sleep_s);
        sys::esp_deep_sleep_start();
    }
    // esp_deep_sleep_start() не возвращается.
    unreachable!()
}
