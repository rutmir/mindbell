//! Модель расписания и вычисление момента следующего срабатывания.
//!
//! Этот модуль — чистый Rust без зависимостей от железа, поэтому его можно
//! гонять юнит-тестами на хосте (`cargo test` в обычном окружении).

use serde::{Deserialize, Serialize};

pub const SECONDS_PER_DAY: u32 = 86_400;

/// Один диапазон расписания (например «день» или «вечер»).
///
/// Время — минуты от локальной полуночи. Диапазон НЕ должен пересекать
/// полночь (`start_min < end_min`); ночной интервал телефон должен разбивать
/// на два сегмента или просто оставлять «дырку» (тишина).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Начало диапазона, минуты от полуночи (0..=1440).
    pub start_min: u16,
    /// Конец диапазона, минуты от полуночи (`> start_min`).
    pub end_min: u16,
    /// Период вибросигнала внутри диапазона, минуты (`> 0`).
    pub interval_min: u16,
    /// Включён ли диапазон.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Schedule {
    pub segments: Vec<Segment>,
}

impl Schedule {
    pub fn from_json(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| b"{\"segments\":[]}".to_vec())
    }

    /// Расписание по умолчанию (используется при первом запуске, пока телефон
    /// не прислал своё): день 9:00–20:00 каждые 7 мин, вечер 20:00–23:00
    /// каждые 20 мин, ночью тишина.
    pub fn default_demo() -> Self {
        Schedule {
            segments: vec![
                Segment { start_min: 9 * 60, end_min: 20 * 60, interval_min: 7, enabled: true },
                Segment { start_min: 20 * 60, end_min: 23 * 60, interval_min: 20, enabled: true },
            ],
        }
    }

    /// Сколько секунд осталось до ближайшего срабатывания СТРОГО после `now_sec`.
    ///
    /// `now_sec` — секунды от локальной полуночи (0..86400). Слоты вибросигнала
    /// выровнены по началу диапазона: `start, start+interval, start+2·interval, …`
    /// пока меньше `end`. Поиск ведётся на двое суток вперёд, чтобы корректно
    /// перепрыгнуть ночную «дырку» до утреннего диапазона.
    ///
    /// `None` — если включённых диапазонов нет вовсе.
    pub fn seconds_until_next(&self, now_sec: u32) -> Option<u32> {
        let mut best: Option<u32> = None;
        for day in 0..2u32 {
            let base = day * SECONDS_PER_DAY;
            for seg in self
                .segments
                .iter()
                .filter(|s| s.enabled && s.interval_min > 0 && s.end_min > s.start_min)
            {
                let start = base + seg.start_min as u32 * 60;
                let end = base + seg.end_min as u32 * 60;
                let step = seg.interval_min as u32 * 60;

                // первый слот строго после now_sec
                let t = if now_sec >= start {
                    let k = (now_sec - start) / step + 1;
                    start + k * step
                } else {
                    start
                };

                if t > now_sec && t < end {
                    let delta = t - now_sec;
                    best = Some(best.map_or(delta, |b| b.min(delta)));
                }
            }
        }
        best
    }

    /// То же, что [`seconds_until_next`], но слоты в пределах `guard_s` секунд
    /// от `now_sec` считаются текущим, УЖЕ отработанным слотом и пропускаются.
    ///
    /// Нужно после вибросигнала по таймеру: из-за коррекции дрейфа RTC
    /// пробуждение может случиться на пару секунд РАНЬШЕ слота, и без защиты
    /// планировщик назначил бы тот же слот повторно (дубль сигнала через
    /// секунду-другую). `guard_s` должен быть меньше минимального периода
    /// (60 с при `interval_min = 1`), чтобы не проглотить соседний слот.
    pub fn seconds_until_next_guarded(&self, now_sec: u32, guard_s: u32) -> Option<u32> {
        // `seconds_until_next` корректно работает и при аргументе чуть больше
        // 86400 (поиск ведётся на двое суток вперёд).
        self.seconds_until_next(now_sec + guard_s).map(|d| d + guard_s)
    }

    /// Находится ли `now_sec` внутри какого-либо активного диапазона
    /// (`start ≤ now < end`). Прошивка использует это, чтобы при пробуждении по
    /// таймеру жужжать ТОЛЬКО внутри рабочего окна, а не на «страховочном»
    /// пробуждении (когда слотов нет вовсе). Проверка по широкому окну (часы)
    /// устойчива к дрейфу RTC: проснувшись даже на минуты позже слота, мы всё
    /// ещё «внутри» диапазона.
    pub fn in_active_segment(&self, now_sec: u32) -> bool {
        self.segments.iter().any(|s| {
            s.enabled
                && s.interval_min > 0
                && s.end_min > s.start_min
                && now_sec >= s.start_min as u32 * 60
                && now_sec < s.end_min as u32 * 60
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(start_h: u16, end_h: u16, interval: u16) -> Segment {
        Segment { start_min: start_h * 60, end_min: end_h * 60, interval_min: interval, enabled: true }
    }

    #[test]
    fn first_slot_at_segment_start() {
        let s = Schedule { segments: vec![day(9, 20, 7)] };
        // 08:00 -> до 09:00 ровно 1 час
        assert_eq!(s.seconds_until_next(8 * 3600), Some(3600));
    }

    #[test]
    fn aligned_to_interval() {
        let s = Schedule { segments: vec![day(9, 20, 7)] };
        // 09:00 -> следующий слот 09:07
        assert_eq!(s.seconds_until_next(9 * 3600), Some(7 * 60));
        // 09:03 -> до 09:07 = 4 мин
        assert_eq!(s.seconds_until_next(9 * 3600 + 3 * 60), Some(4 * 60));
    }

    #[test]
    fn jumps_to_next_segment_after_gap() {
        // день до 20:00, вечер с 20:00 — проверим переход на вечер
        let s = Schedule { segments: vec![day(9, 20, 7), day(20, 23, 20)] };
        // 19:58 — следующий день-слот был бы после 20:00, значит берём вечер 20:00
        let now = 19 * 3600 + 58 * 60;
        assert_eq!(s.seconds_until_next(now), Some(2 * 60));
    }

    #[test]
    fn wraps_to_next_morning_at_night() {
        let s = Schedule { segments: vec![day(9, 20, 7)] };
        // 23:00 — сегодня уже всё, ближайший слот завтра в 09:00
        let now = 23 * 3600;
        let expected = (SECONDS_PER_DAY - now) + 9 * 3600;
        assert_eq!(s.seconds_until_next(now), Some(expected));
    }

    #[test]
    fn disabled_segment_ignored() {
        let mut seg = day(9, 20, 7);
        seg.enabled = false;
        let s = Schedule { segments: vec![seg] };
        assert_eq!(s.seconds_until_next(8 * 3600), None);
    }

    #[test]
    fn guarded_skips_current_slot_on_early_wake() {
        let s = Schedule { segments: vec![day(9, 20, 7)] };
        // Проснулись на 2 с раньше слота 09:07 (коррекция дрейфа отмотала часы),
        // отвибрировали. Без защиты следующим был бы тот же слот через 2 с —
        // с защитой берём 09:14.
        let now = 9 * 3600 + 7 * 60 - 2;
        assert_eq!(s.seconds_until_next(now), Some(2)); // сам дубль
        assert_eq!(s.seconds_until_next_guarded(now, 30), Some(7 * 60 + 2));
    }

    #[test]
    fn guarded_keeps_normal_next_slot() {
        let s = Schedule { segments: vec![day(9, 20, 7)] };
        // Проснулись ровно на слоте 09:07 — следующий 09:14, защита ничего не меняет.
        let now = 9 * 3600 + 7 * 60;
        assert_eq!(s.seconds_until_next_guarded(now, 30), s.seconds_until_next(now));
    }

    #[test]
    fn guarded_wraps_past_midnight() {
        // Слот у самой полуночи: now + guard уходит за 86400 — поиск на двое
        // суток вперёд должен это переварить.
        let s = Schedule { segments: vec![day(9, 24, 7)] };
        let now = SECONDS_PER_DAY - 2; // 23:59:58, сегодняшние слоты кончились
        let expected = 2 + 9 * 3600; // завтра 09:00
        assert_eq!(s.seconds_until_next_guarded(now, 30), Some(expected));
    }

    #[test]
    fn in_active_segment_window() {
        let s = Schedule { segments: vec![day(9, 20, 7)] };
        assert!(s.in_active_segment(9 * 3600)); // ровно начало — внутри
        assert!(s.in_active_segment(12 * 3600)); // середина
        assert!(!s.in_active_segment(8 * 3600)); // до начала
        assert!(!s.in_active_segment(20 * 3600)); // конец исключается
        assert!(!s.in_active_segment(23 * 3600)); // ночь
    }

    #[test]
    fn in_active_segment_ignores_disabled() {
        let mut seg = day(9, 20, 7);
        seg.enabled = false;
        let s = Schedule { segments: vec![seg] };
        assert!(!s.in_active_segment(12 * 3600));
    }

    #[test]
    fn json_roundtrip() {
        let s = Schedule::default_demo();
        let bytes = s.to_json();
        assert_eq!(Schedule::from_json(&bytes).unwrap(), s);
    }
}
