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
//!     через deep sleep; расписание оперирует только временем суток;
//!   * телефон при синке шлёт UTC + смещение зоны раздельно: UTC используется
//!     для автокалибровки дрейфа RC-осциллятора (монотонен → устойчив к смене
//!     часового пояса/летнего времени), смещение — для локального времени суток
//!     (см. секцию «Калибровка дрейфа RTC»).

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

/// Ключ калибровки дрейфа RTC в NVS. Blob 12 байт: [0..4] cal_ppm (i32 LE),
/// [4..12] время последнего синка (i64 LE, «локальный epoch» сек).
const NVS_CLKCAL_KEY: &str = "clkcal";

/// Порог «системное время выставлено» (≈ 2020-09): меньше — часы ещё не шли.
const TIME_SET_THRESHOLD: i64 = 1_600_000_000;

/// Минимальный реальный интервал между двумя синками, чтобы оценка дрейфа была
/// достоверной (короткие интервалы дают шумный ppm из-за ±1с дискретизации).
const MIN_CAL_INTERVAL_S: i64 = 4 * 3600;

/// Ограничение единичного шага коррекции (страховка от выброса), ppm.
const MAX_CAL_STEP_PPM: i64 = 20_000;

/// Полное ограничение cal_ppm: внутренний RC C3 укладывается в ±5%.
const MAX_CAL_PPM: i64 = 50_000;

/// После вибросигнала по таймеру слоты ближе этого порога считаем текущим, уже
/// отработанным слотом (см. Schedule::seconds_until_next_guarded): коррекция
/// дрейфа может отмотать часы на пару секунд ЗА слот, и без защиты тот же слот
/// сработал бы повторно. Должен быть меньше минимального периода (60 с при
/// interval_min = 1), чтобы не проглотить соседний легитимный слот.
const REBUZZ_GUARD_S: u32 = 30;

// UUID сервиса и характеристик (произвольные 128-бит). Те же должны быть в
// приложении на Flutter.
const SERVICE_UUID: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000000");
/// Чтение/запись расписания (JSON, как в schedule.rs).
const CHAR_CONFIG: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000001");
/// Синхронизация времени (запись, little-endian). Новый формат — 10 байт:
/// u64 UTC-epoch (сек) + i16 смещение зоны (мин), локальное = UTC + offset·60.
/// UTC идёт в калибровку дрейфа (монотонен, иммунен к смене зоны), offset — в
/// расписание. Легаси-формат 8 байт (u64 «локальный epoch») принимается, но без
/// калибровки дрейфа. См. on_time_sync / on_time_sync_legacy.
const CHAR_TIME: BleUuid = uuid128!("6d696e64-6265-6c6c-0000-000000000003");
// Примечание: характеристика батареи (…0002) зарезервирована на будущее —
// измерение заряда пока не разведено (см. hardware_notes.md §5).

// ─────────────────────────────────────────────────────────────────────────────

/// Точка отсчёта коррекции дрейфа: показание скорректированных ЛОКАЛЬНЫХ часов в
/// момент последней коррекции/синка. В RTC-памяти → переживает deep sleep,
/// обнуляется при холодном старте/потере питания. 0 = «точки нет» (часы ещё не
/// шли). Если RTC-память почему-то не сохранится — поправка просто не применится
/// (безопасная деградация к нынешнему поведению), см. apply_drift_correction.
#[link_section = ".rtc.data"]
static mut CLOCK_CHECKPOINT: i64 = 0;

/// Дробный остаток коррекции дрейфа, ещё не применённый к часам, в микродолях
/// секунды (1_000_000 = 1 с), всегда в [0, 1_000_000). Часы правятся целыми
/// секундами; без переноса остатка floor() терял бы до ~1 с на каждом
/// пробуждении, калибровка компенсировала бы это завышением cal_ppm на ~5–7%,
/// и между синками сигнал уходил бы от истинного времени на минуты. Живёт в
/// RTC-памяти в паре с CLOCK_CHECKPOINT; при потере обнуляется — безопасно.
#[link_section = ".rtc.data"]
static mut CORR_REMAINDER: i64 = 0;

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

    // Коррекция дрейфа RC: правим часы на накопленную ошибку СРАЗУ после сна, до
    // любых решений по расписанию. cal_ppm обучается при синхронизации с телефоном
    // (on_time_sync). При cal_ppm=0 (ещё не калибровались) — это no-op.
    let (cal_ppm, _, _) = load_clkcal(&mut nvs);
    apply_drift_correction(cal_ppm);

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

    // Был ли на этом пробуждении штатный сигнал по таймеру — тогда при пересчёте
    // расписания текущий слот считаем отработанным (REBUZZ_GUARD_S).
    let mut buzzed_slot = false;

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
                    buzzed_slot = true;
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
    // когда нас будить. Если только что вибрировали — текущий слот отработан.
    let schedule = load_schedule(&mut nvs);
    let next = now_local_sec().and_then(|now| {
        if buzzed_slot {
            schedule.seconds_until_next_guarded(now, REBUZZ_GUARD_S)
        } else {
            schedule.seconds_until_next(now)
        }
    });
    match next {
        Some(s) => log::info!("next buzz in {} s", s),
        None => log::warn!("no time/segments — sleeping until button"),
    }

    // cal_ppm перечитываем: режим настройки мог его обновить (on_time_sync).
    let (cal_ppm, _, _) = load_clkcal(&mut nvs);
    enter_deep_sleep(next, cal_ppm);
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

// ── Калибровка дрейфа RTC ─────────────────────────────────────────────────────
//
// Внутренний RC-осциллятор C3 убегает на ~0.7%/сут. Идея: телефон при синке даёт
// эталон, по двум синкам устройство само вычисляет свой ppm (Часть A) и затем при
// каждом пробуждении вычитает накопленную ошибку из часов (Часть B).
//
// КЛЮЧЕВОЕ для устойчивости к смене зоны: дрейф считается по UTC (монотонен), а
// НЕ по локальному времени. Локальное время = UTC + смещение зоны; смещение
// прыгает при поездках/переходе на летнее время и сломало бы оценку, если бы мы
// мерили по локальной дельте. Поэтому телефон шлёт (UTC, offset_min) раздельно:
// UTC идёт в калибровку, offset — только для расписания (часы суток).

/// Загружает калибровку: (cal_ppm, время последнего синка в UTC, offset_min).
/// При отсутствии/повреждении — нули (калибровка ещё не велась).
fn load_clkcal(nvs: &mut EspNvs<NvsDefault>) -> (i32, i64, i16) {
    let mut buf = [0u8; 14];
    match nvs.get_blob(NVS_CLKCAL_KEY, &mut buf) {
        Ok(Some(b)) if b.len() >= 14 => {
            let ppm = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            let last = i64::from_le_bytes([b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11]]);
            let off = i16::from_le_bytes([b[12], b[13]]);
            (ppm, last, off)
        }
        _ => (0, 0, 0),
    }
}

fn store_clkcal(nvs: &mut EspNvs<NvsDefault>, cal_ppm: i32, last_sync_utc: i64, offset_min: i16) {
    let mut buf = [0u8; 14];
    buf[0..4].copy_from_slice(&cal_ppm.to_le_bytes());
    buf[4..12].copy_from_slice(&last_sync_utc.to_le_bytes());
    buf[12..14].copy_from_slice(&offset_min.to_le_bytes());
    if let Err(e) = nvs.set_blob(NVS_CLKCAL_KEY, &buf) {
        log::warn!("clkcal: failed to persist ({e})");
    }
}

/// Часть B. Поправляет системные часы на накопленный дрейф RC. Вызывать сразу
/// после пробуждения, ДО решений по расписанию. Работает с ЛОКАЛЬНЫМИ часами:
/// поправка — это коррекция скорости (ppm·прошедшее_время), она не зависит от
/// смещения зоны (смещение постоянно между синками). Безопасна при невыставленном
/// времени и при отсутствии точки отсчёта.
fn apply_drift_correction(cal_ppm: i32) {
    let mut tv = sys::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
    let now = tv.tv_sec as i64;
    if now < TIME_SET_THRESHOLD {
        return; // часы ещё не синхронизированы — корректировать нечего
    }
    let cp = unsafe { core::ptr::addr_of!(CLOCK_CHECKPOINT).read() };
    if cp == 0 {
        // Нет точки отсчёта (холодный старт/потеря питания): заводим и выходим.
        unsafe { core::ptr::addr_of_mut!(CLOCK_CHECKPOINT).write(now) };
        unsafe { core::ptr::addr_of_mut!(CORR_REMAINDER).write(0) };
        return;
    }
    let raw_delta = now - cp;
    if raw_delta <= 0 {
        return;
    }
    // cal_ppm>0 → часы спешат → вычитаем набежавшее (raw_delta·ppm) плюс остаток
    // с прошлых пробуждений. div/rem_euclid: corr округляется вниз, остаток
    // всегда в [0, 1e6) и применится позже — суммарная коррекция точная при
    // любом знаке cal_ppm, ничего не теряется на округлении.
    let acc = unsafe { core::ptr::addr_of!(CORR_REMAINDER).read() } + raw_delta * cal_ppm as i64;
    let corr = acc.div_euclid(1_000_000);
    unsafe { core::ptr::addr_of_mut!(CORR_REMAINDER).write(acc.rem_euclid(1_000_000)) };
    if corr == 0 {
        // Накопили < 1 с: остаток уже перенесён в CORR_REMAINDER, точку отсчёта
        // двигаем (скорректированные часы == сырые).
        unsafe { core::ptr::addr_of_mut!(CLOCK_CHECKPOINT).write(now) };
        return;
    }
    let corrected = now - corr;
    let tv2 = sys::timeval { tv_sec: corrected as sys::time_t, tv_usec: tv.tv_usec };
    unsafe { sys::settimeofday(&tv2, core::ptr::null()) };
    unsafe { core::ptr::addr_of_mut!(CLOCK_CHECKPOINT).write(corrected) };
    log::info!("drift corr: raw_delta={}s cal_ppm={} -> clock -= {}s", raw_delta, cal_ppm, corr);
}

/// Часть A. Применяет синхронизацию (UTC + смещение зоны) и калибрует дрейф.
/// `utc_epoch` — эталонный UTC от телефона; `offset_min` — смещение зоны в минутах
/// (локальное = UTC + offset). Дрейф оценивается по UTC → иммунитет к смене зоны.
fn on_time_sync(nvs: &mut EspNvs<NvsDefault>, utc_epoch: u64, offset_min: i16) {
    let utc = utc_epoch as i64;

    // Текущее ЛОКАЛЬНОЕ показание часов ДО перезаписи.
    let mut tv = sys::timeval { tv_sec: 0, tv_usec: 0 };
    unsafe { sys::gettimeofday(&mut tv, core::ptr::null_mut()) };
    let dev_local = tv.tv_sec as i64;

    let (mut cal_ppm, last_utc, last_offset) = load_clkcal(nvs);

    // Калибруем, только если был корректный прошлый синк и часы реально шли.
    if last_utc >= TIME_SET_THRESHOLD && dev_local >= TIME_SET_THRESHOLD {
        // Восстанавливаем UTC, как его «видело» устройство, через смещение,
        // действовавшее ВО ВРЕМЯ интервала (last_offset). Так смена зоны на этом
        // синке не попадает в ошибку — сравниваем UTC с UTC.
        let dev_utc = dev_local - (last_offset as i64) * 60;
        let real_elapsed = utc - last_utc;
        if real_elapsed >= MIN_CAL_INTERVAL_S {
            let dev_error = dev_utc - utc; // + = часы спешили (после прошлой коррекции)
            let residual = ((dev_error as i128 * 1_000_000) / real_elapsed as i128) as i64;
            let residual = residual.clamp(-MAX_CAL_STEP_PPM, MAX_CAL_STEP_PPM);
            let updated = ((cal_ppm as i64) + residual).clamp(-MAX_CAL_PPM, MAX_CAL_PPM);
            log::info!(
                "clock cal: elapsed={}s dev_error={}s residual={}ppm, cal_ppm {} -> {}",
                real_elapsed, dev_error, residual, cal_ppm, updated
            );
            cal_ppm = updated as i32;
        } else {
            log::info!(
                "clock cal: interval {}s < {}s — too short, skip",
                real_elapsed, MIN_CAL_INTERVAL_S
            );
        }
    }

    // Ставим локальное время и заводим от него точку отсчёта коррекции;
    // недоприменённый остаток относился к старой точке — сбрасываем.
    let new_local = utc + (offset_min as i64) * 60;
    set_local_time(new_local.max(0) as u64);
    unsafe { core::ptr::addr_of_mut!(CLOCK_CHECKPOINT).write(new_local) };
    unsafe { core::ptr::addr_of_mut!(CORR_REMAINDER).write(0) };
    store_clkcal(nvs, cal_ppm, utc, offset_min);
}

/// Легаси-синк: телефон прислал только локальный epoch (8 байт), без UTC/зоны.
/// TZ-безопасно откалибровать дрейф нельзя → калибровку ставим на паузу
/// (last_sync_utc=0), но уже выученный cal_ppm продолжаем применять и часы ставим.
fn on_time_sync_legacy(nvs: &mut EspNvs<NvsDefault>, local_epoch: u64) {
    set_local_time(local_epoch);
    unsafe { core::ptr::addr_of_mut!(CLOCK_CHECKPOINT).write(local_epoch as i64) };
    unsafe { core::ptr::addr_of_mut!(CORR_REMAINDER).write(0) };
    let (cal_ppm, _, _) = load_clkcal(nvs);
    store_clkcal(nvs, cal_ppm, 0, 0);
    log::warn!("legacy time sync (no tz offset): drift learning paused, set cal_ppm kept");
}

// ── Режим настройки (BLE) ─────────────────────────────────────────────────────

fn run_config_mode(nvs: &mut EspNvs<NvsDefault>) -> anyhow::Result<()> {
    // Буферы, в которые BLE-колбэки складывают присланное; применяем после окна.
    let pending_sched: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    // Сырые байты CHAR_TIME: 10 байт = UTC(u64 LE) + offset_min(i16 LE) [новый
    // формат], либо 8 байт = локальный epoch(u64 LE) [легаси]. Разбор — после окна.
    let pending_time: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
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
                *p.lock().unwrap() = Some(d.to_vec());
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
    if let Some(bytes) = pending_time.lock().unwrap().take() {
        if bytes.len() >= 10 {
            // Новый формат: UTC + смещение зоны → калибровка дрейфа (TZ-безопасно).
            let utc = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
            let offset_min = i16::from_le_bytes(bytes[8..10].try_into().unwrap());
            on_time_sync(nvs, utc, offset_min);
        } else {
            // Легаси: только локальный epoch → калибровку не ведём (см. функцию).
            let local = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
            on_time_sync_legacy(nvs, local);
        }
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
///
/// `cal_ppm` — выученный дрейф часов (см. «Калибровка дрейфа RTC»): длительность
/// таймера предкомпенсируется, чтобы ПОСЛЕ отмотки часов в
/// apply_drift_correction проснуться ровно на слоте, а не за пару секунд до него.
fn enter_deep_sleep(next_secs: Option<u32>, cal_ppm: i32) -> ! {
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

        // Предкомпенсация дрейфа: при cal_ppm > 0 часы спешат, и коррекция после
        // пробуждения отмотает их назад на sleep·ppm — без компенсации мы бы
        // проснулись ДО слота (источник дублей сигнала). Спим в «сырых» единицах
        // дольше: raw = s / (1 − ppm/1e6); тогда скорректированные часы за сон
        // продвинутся ровно на s. Знаменатель > 0: |cal_ppm| ≤ MAX_CAL_PPM (5%).
        let sleep_us =
            (sleep_s as i128 * 1_000_000 * 1_000_000 / (1_000_000 - cal_ppm as i128)) as u64;
        sys::esp_sleep_enable_timer_wakeup(sleep_us);

        log::info!(
            "entering deep sleep for {} s (raw {} us, cal_ppm={}, gpio+timer armed)",
            sleep_s, sleep_us, cal_ppm
        );
        sys::esp_deep_sleep_start();
    }
    // esp_deep_sleep_start() не возвращается.
    unreachable!()
}
